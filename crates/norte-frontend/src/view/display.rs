//! Name sanitisation for painting in any frontend: a name is bytes (spec
//! §6) and the text that gets painted is ALWAYS lossy and MARKED — never
//! silent loss, never raw controls/bidi.

use norte_proto::VPath;
use unicode_width::UnicodeWidthChar;

/// Should this be masked in a terminal? DELEGATES to
/// [`norte_encoding::is_terminal_hazard`] (the SINGLE source for the set —
/// it used to live trapped here; now it is shared with `fs.search`'s
/// preview sanitisation). Covers Cc (controls: `\n`, ESC — ratatui SILENTLY
/// DELETES them and a direct frontend would execute them), the bidi Cf
/// overrides (RTL visual-order spoofing), and the INVISIBLE Cf/Zl/Zp
/// (encoding-auditor H4 of M3-3b: two visually identical names that differ
/// in bytes trick a human who approves "the one I already saw"). The
/// invisibles are decided by the Unicode `Default_Ignorable_Code_Point`
/// property plus the ones that paint blank without being one (BRAILLE
/// BLANK, the interlinear annotations, Zl/Zp) — it used to be a hand-written
/// list that left out the Hangul fillers, which are not even Cf (#125). ZWJ
/// (U+200D) and the variation selectors are PERMITTED knowingly: masking
/// them would break legitimate composed emoji (fixture `emoji_zwj_family`)
/// — emoji fidelity > the residual of an invisible twin.
fn must_mask(c: char) -> bool {
    norte_encoding::is_terminal_hazard(c)
}

/// A name ready to paint: `(text, hostile)`. `hostile = true` when the
/// painted text DIFFERS from the real name: non-UTF8 bytes (lossy `�`),
/// controls or bidi masked to `�` (spec §6: display is always lossy and
/// MARKED — never silent loss, never raw controls).
#[must_use]
pub fn display_name(bytes: &[u8]) -> (String, bool) {
    let (raw, lossy) = match std::str::from_utf8(bytes) {
        Ok(s) => (std::borrow::Cow::Borrowed(s), false),
        Err(_) => (String::from_utf8_lossy(bytes), true),
    };
    let mut masked = false;
    let text: String = raw
        .chars()
        .map(|c| {
            if must_mask(c) {
                masked = true;
                '\u{FFFD}'
            } else {
                c
            }
        })
        .collect();
    (text, lossy || masked)
}

/// [`display_name`] for a FILESYSTEM name.
///
/// An [`std::ffi::OsStr`] is not text, and on Unix it is bytes: it is
/// painted through the same path as any other name —lossy MARKED, hazards
/// masked— without touching the bytes the file is opened with. On Windows
/// there are no bytes to extract without going through UTF-16, so the
/// platform's lossy conversion is used and the flag is set the same way.
///
/// ```
/// use std::ffi::OsStr;
/// use norte_frontend::display_os_name;
///
/// assert_eq!(display_os_name(OsStr::new("mio")), ("mio".to_owned(), false));
/// ```
#[must_use]
pub fn display_os_name(name: &std::ffi::OsStr) -> (String, bool) {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        display_name(name.as_bytes())
    }
    #[cfg(not(unix))]
    {
        let text = name.to_string_lossy();
        let (painted, hostile) = display_name(text.as_bytes());
        (
            painted,
            hostile || matches!(text, std::borrow::Cow::Owned(_)),
        )
    }
}

/// [`display_name`] with optional REINTERPRETATION (#57, spec §6.1): with
/// `Some(enc)`, a NON-UTF8 name is decoded with `enc` for display instead of
/// falling back to lossy `�` — the bytes are never mutated (rule 1) and the
/// hostile flag stays `true` (the painted text DIFFERS from the real name:
/// it is a VIEW the user chose, the badge gives it away all the same).
///
/// A VALID UTF-8 name is never reinterpreted: it is already text — in a
/// mixed container (UTF-8 entries + cp866 entries), reinterpreting the UTF-8
/// ones would manufacture mojibake where there was no problem. Hazard
/// masking applies the same on both paths.
///
/// ```
/// use norte_encoding::NameEncoding;
/// use norte_frontend::display_name_with;
/// // Non-UTF8 in cp437: decodes AND flags (the text is not the bytes).
/// let (text, hostile) = display_name_with(b"CAF\x90.TXT", Some(NameEncoding::Cp437));
/// assert_eq!((text.as_str(), hostile), ("CAFÉ.TXT", true));
/// // Valid UTF-8: NEVER reinterpreted (mixed container, no mojibake).
/// let (text, hostile) = display_name_with("año.txt".as_bytes(), Some(NameEncoding::Cp437));
/// assert_eq!((text.as_str(), hostile), ("año.txt", false));
/// ```
#[must_use]
pub fn display_name_with(
    bytes: &[u8],
    reinterpret: Option<norte_encoding::NameEncoding>,
) -> (String, bool) {
    let (Some(enc), Err(_)) = (reinterpret, std::str::from_utf8(bytes)) else {
        return display_name(bytes);
    };
    let decoded = norte_encoding::decode_name(bytes, enc);
    let text: String = decoded
        .chars()
        .map(|c| if must_mask(c) { '\u{FFFD}' } else { c })
        .collect();
    (text, true)
}

/// A full path ready to paint: `⟨scheme authority⟩/` prefix **except for
/// `file` with no authority**, which is not announced (see
/// [`path_display_with`]) + each segment through [`display_name`], and
/// flagged if ANY segment would come out altered.
///
/// No longer mirrors `VPath::display_lossy`, and that is deliberate:
/// `display_lossy` is a LOG's and an error's form —where the scheme always
/// matters, because there is no screen around it to say so— and this is a
/// screen's, where the default scheme is noise on every row. With any other
/// scheme the two still agree.
///
/// The text is built segment by segment with `display_name` (not with
/// `display_lossy`, encoding review MEDIA-2): the TEXT's masking criterion
/// is the SAME as the flag's — `display_lossy` only covers Cc+bidi and left
/// ZWSP/TAG raw (identical invisible twins, both badged).
/// Note on ZWNJ: proto PERMITS it in `display_lossy` (legitimate in
/// Persian); `must_mask` masks it — here `must_mask` wins knowingly: in the
/// TUI, an invisible twin on a decision surface weighs more than
/// typographic fidelity (the badge already gives away the alteration).
#[must_use]
pub fn path_display(p: &VPath) -> (String, bool) {
    path_display_with(p, None)
}

/// [`path_display`] with optional reinterpretation (#98/F2): each segment
/// goes through [`display_name_with`] — the DECISION surfaces (confirm/
/// collision modals, the viewer's title, the bar's dir) show the same text
/// the user navigates by, not the raw lossy one. Same badge contract: any
/// altered segment (reinterpretation included) flags it.
///
/// **`file` with no authority is not announced.** It is the default case
/// —this machine, this disk— so its label distinguishes nothing at all: it
/// used to be painted on every path of every listing, header, and modal,
/// spending eight columns exactly where space is scarce. What is
/// informative is the scheme that is NOT the usual one, and those still get
/// said. A `file` WITH authority too: then it is another machine, and that
/// has to be said.
///
/// ```
/// use norte_encoding::NameEncoding;
/// use norte_frontend::path_display_with;
/// let p = norte_proto::VPath::parse("mem:///CAF%90.TXT").unwrap();
/// let (text, hostile) = path_display_with(&p, Some(NameEncoding::Cp437));
/// assert_eq!((text.as_str(), hostile), ("⟨mem⟩/CAFÉ.TXT", true));
///
/// // A local one reads the way anyone would write it.
/// let local = norte_proto::VPath::parse("file:///home/o/notas.txt").unwrap();
/// assert_eq!(path_display_with(&local, None).0, "/home/o/notas.txt");
/// ```
#[must_use]
pub fn path_display_with(
    p: &VPath,
    reinterpret: Option<norte_encoding::NameEncoding>,
) -> (String, bool) {
    let mut out = String::new();
    if p.scheme() != "file" || p.authority().is_some() {
        out.push('⟨');
        out.push_str(p.scheme());
        if let Some(a) = p.authority() {
            out.push(' ');
            out.push_str(a);
        }
        out.push('⟩');
    }
    out.push('/');
    let mut hostile = false;
    let mut first = true;
    for seg in p.segments() {
        if !first {
            out.push('/');
        }
        first = false;
        let (text, h) = display_name_with(seg, reinterpret);
        hostile |= h;
        out.push_str(&text);
    }
    (out, hostile)
}

/// `s`'s DISPLAY width in terminal cells.
///
/// The same measure [`middle_ellipsis`] budgets against, exposed to the
/// crate so whoever reserves room for a LATER field measures it the same
/// way the truncator does.
///
/// It is summed PER CHARACTER, and that is not the width `unicode-width`
/// gives the whole string: a VS16 or a pair of regional indicators paint
/// less than the sum of their parts. This is deliberate — this module's
/// walkers can only count per character, and a measure that does not count
/// like they do leaves budgets that do not match the truncation.
///
/// `width()` returns `None` only for CONTROLS, and here they weigh 0. Every
/// caller of this module has already masked them to `�` before measuring.
#[must_use]
pub fn cells(s: &str) -> usize {
    s.chars()
        .map(|c| UnicodeWidthChar::width(c).unwrap_or(0))
        .sum()
}

/// Drops the first `n` CELLS of `s` and returns the rest, aligned.
///
/// This is the viewer's horizontal scroll: the window starts at column `n`,
/// and "column" in a terminal is a cell, not a byte or a character.
/// Counting by bytes would jump the text around the moment it hit an
/// accent; counting by characters would misalign any line with CJK, where
/// one character takes up two columns.
///
/// **A WIDE character straddling the cut goes ENTIRELY and leaves its gap
/// blank.** Its left half cannot be painted —there is no half ideogram—,
/// but simply dropping it shifts that row one column relative to the ones
/// next to it, and that breaks exactly what a horizontal scroll is for
/// reading: a CSV, an aligned log, a table. The space returned in its place
/// is what keeps the grid.
///
/// **The zero-width marks that would open the rest are DROPPED**, the same
/// as [`middle_ellipsis`] and [`ellipsis_at_bytes`] do at the tail: they
/// have lost their base by construction, and leaving them re-parents them
/// to the next letter. A ZWJ cluster cut in half would otherwise paint a
/// different family than the one in the file.
///
/// Width is counted PER CHARACTER, same as [`cells`], and that is
/// deliberate even though it is not the width `unicode-width` would give the
/// whole string (a VS16 or a pair of regional indicators paint less than
/// the sum of their parts): whoever walks the string can only count per
/// character, and a bound that does not count the same way the walker does
/// lets the scroll reach where the walker does not.
///
/// ```
/// use norte_frontend::display::skip_cells;
///
/// assert_eq!(skip_cells("hola", 0), "hola");
/// assert_eq!(skip_cells("hola", 2), "la");
/// assert_eq!(skip_cells("hola", 9), "");
/// // An ideogram takes up TWO cells: cutting it at the first one takes it
/// // whole, and its gap stays blank so the row does not shift.
/// assert_eq!(skip_cells("漢字", 1), " 字");
/// assert_eq!(skip_cells("漢字", 2), "字");
/// ```
#[must_use]
pub fn skip_cells(s: &str, n: usize) -> String {
    if n == 0 {
        return s.to_owned();
    }
    let mut skipped = 0usize;
    let mut rest = "";
    for (i, c) in s.char_indices() {
        if skipped >= n {
            rest = &s[i..];
            break;
        }
        skipped += UnicodeWidthChar::width(c).unwrap_or(0);
    }
    // What was skipped in EXCESS is half of a wide character that did not
    // fit: its gap goes blank.
    let gap = skipped.saturating_sub(n);
    // And the orphaned marks at the start, out: their base stayed on the
    // other side of the cut.
    let rest = strip_leading_marks(rest);
    let mut out = String::with_capacity(gap + rest.len());
    for _ in 0..gap {
        out.push(' ');
    }
    out.push_str(rest);
    out
}

/// What is left of `s` after dropping the ZERO-width marks from its head.
///
/// A leading mark lost its base on the other side of a cut from the left,
/// and leaving it re-parents it to the next letter: a ZWJ cluster split in
/// half would paint a glyph that is not in the file. It is the same rule
/// [`middle_ellipsis`] and [`ellipsis_at_bytes`] apply to the TAIL, for the
/// same reason and at the other end.
///
/// Public within the crate because the viewer's styled truncation has to
/// apply it at the same cut point as the whole string's; two different
/// answers to this are two different paintings of the same file.
pub(crate) fn strip_leading_marks(s: &str) -> &str {
    s.trim_start_matches(|c| UnicodeWidthChar::width(c) == Some(0))
}

/// Truncates to a BYTE cap at the tail, marking the cut with `…`.
///
/// This is the truncator for boundaries measured in bytes —the graphical
/// host's bridge, a message field— and not for ones measured in cells, which
/// is what [`middle_ellipsis`] does. They share what matters: nothing is
/// lost silently (the `…` says so) and a cluster is not split.
///
/// The cut falls on a CHARACTER boundary and then backs up while whatever is
/// left at the end is zero-width —a combining mark, a ZWJ, a variation
/// selector—: otherwise the `…` added afterwards composes with the orphaned
/// mark and the accent moves from its letter to the ellipsis (the same FIX
/// 3(b) this module already applies at `middle_ellipsis`'s tail), or an
/// emoji family gets cut at its joiner and paints as separate people.
///
/// ```
/// use norte_frontend::display::ellipsis_at_bytes;
///
/// assert_eq!(ellipsis_at_bytes("hola", 16), "hola");
/// let truncated = ellipsis_at_bytes(&"a".repeat(100), 10);
/// assert!(truncated.len() <= 10 && truncated.ends_with('…'));
/// ```
#[must_use]
pub fn ellipsis_at_bytes(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_owned();
    }
    let ellipsis = '…';
    let Some(budget) = max_bytes.checked_sub(ellipsis.len_utf8()) else {
        // Not even the mark fits: better nothing than a lone mark, which
        // does not say what was cut.
        return String::new();
    };
    let mut cut = budget;
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    let mut truncated = &s[..cut];
    // And backwards while the last one is zero-width: those marks have
    // already lost their base, and leaving them re-parents them to the `…`.
    while let Some(c) = truncated.chars().next_back() {
        if UnicodeWidthChar::width(c).unwrap_or(0) > 0 {
            break;
        }
        truncated = &truncated[..truncated.len() - c.len_utf8()];
    }
    let mut out = truncated.to_owned();
    out.push(ellipsis);
    out
}

/// MIDDLE ellipsis at `max` terminal CELLS: keeps the head (scheme) and the
/// tail (name) —what identifies the path to a human— and marks the cut with
/// `…`. Budgets by CELL WIDTH (CJK/emoji take up 2 columns), not by chars:
/// counting chars overflowed `max` with dense names and ratatui re-truncated
/// from the RIGHT, eating exactly the tail the middle ellipsis exists to
/// preserve (#79). For ASCII (cells == chars) the result is identical to the
/// previous one.
///
/// SHARED by both frontends (encoding audit M4-IA-2 H1): it used to live
/// private in the TUI, but the reason it exists is not cosmetic nor specific
/// to a terminal — it is that a mile-long path must NEVER push the field
/// that comes AFTER it (a semantic hit's score, a plan's `→ destination`)
/// out of the box. The GUI relied on the div's `.truncate()`, which cuts
/// from the right silently: a long path with a `· 0.99` embedded (printable
/// chars, no badge) left ONLY the fake score visible. In GPUI the cell
/// budget does not measure pixels, but it is a CONSERVATIVE bound (a wide
/// char counts 2) and enough to reserve room for the tail field.
///
/// P1 encoding audit F2 (LOW): a CHAR-COUNT backstop before the width
/// walker. A combining mark (`U+0301`…) or a ZWJ weighs ZERO cells — a flood
/// of millions of them stuck to a single visible char has total width ≤
/// `max` (the early-return below would return it INTACT, cutting nothing)
/// or, if it overflows because of the visible char, the head/tail walker
/// would keep accumulating zero-width chars without ever touching its
/// budget — in no case is the STRING's size (memory, upstream
/// `display_name`/render work) bounded by `max`, even though the WIDTH is.
/// If `s` carries more than `4*max` chars, it is pre-truncated by CHARS
/// (generous: well above any real cell `max` the TUI has today) to
/// head+tail BEFORE measuring anything — the rest of the function stayed the
/// same over that already-bounded input.
///
/// ```
/// use norte_frontend::middle_ellipsis;
/// // What already fits comes back INTACT.
/// assert_eq!(middle_ellipsis("file:///d/a.txt", 46), "file:///d/a.txt");
/// // What overflows keeps head and tail, and MARKS the cut.
/// let out = middle_ellipsis("file:///muy/larga/ruta/hacia/final.txt", 20);
/// assert!(out.starts_with("file:") && out.ends_with(".txt") && out.contains('…'));
/// ```
#[must_use]
pub fn middle_ellipsis(s: &str, max: usize) -> String {
    if max == 0 {
        // review #108-5 M2: with a 0 budget it used to return "…" (width 1 >
        // 0) and broke the caller's invariant by one cell.
        return String::new();
    }
    let char_cap = max.saturating_mul(4);
    let chars: Vec<char> = s.chars().collect();
    // `true` if the backstop had to discard chars by COUNT (not by width) —
    // in that case the ellipsis is FORCED further below even if the
    // resulting width fits in `max`: spec §6, never silent loss. Without
    // this, a flood of zero-width chars truncated to `char_cap` could end up
    // weighing 0 cells and be returned INTACT (already truncated, but
    // unmarked) by the width early-return.
    let (chars, cut_by_chars) = if chars.len() > char_cap {
        let head_n = char_cap / 2;
        let tail_n = char_cap - head_n;
        let truncated: Vec<char> = chars[..head_n]
            .iter()
            .chain(chars[chars.len() - tail_n..].iter())
            .copied()
            .collect();
        (truncated, true)
    } else {
        (chars, false)
    };
    let cell = |c: char| UnicodeWidthChar::width(c).unwrap_or(0);
    if !cut_by_chars && chars.iter().copied().map(cell).sum::<usize>() <= max {
        return chars.into_iter().collect();
    }
    let s = &chars[..];
    // One cell for the `…`; the rest splits head/tail. Each half
    // accumulates chars while the next one FITS whole in its budget: a wide
    // char that does not fit is discarded (a cell is never split).
    let budget = max.saturating_sub(1);
    let head_budget = budget / 2;
    let tail_budget = budget - head_budget;

    let mut head = String::new();
    let mut used = 0usize;
    for &c in s {
        let w = cell(c);
        if used + w > head_budget {
            break;
        }
        used += w;
        head.push(c);
    }

    let mut tail: Vec<char> = Vec::new();
    let mut used_tail = 0usize;
    for &c in s.iter().rev() {
        let w = cell(c);
        if used_tail + w > tail_budget {
            break;
        }
        used_tail += w;
        tail.push(c);
    }
    tail.reverse();
    // H3b encoding audit, FIX 3(b): the tail walker breaks on the first char
    // that does not FIT, and a combining mark is width 0 — it never breaks.
    // So when the base character it belongs to is the one that overflows, the
    // tail starts with a bare `U+0301`, which the terminal composes onto the
    // `…` we are about to write: the accent migrates from its letter to the
    // ellipsis. The same shape leaves a ZWJ emoji cluster starting on its
    // joiner. Leading zero-width chars in the tail have lost their base by
    // construction, so they are dropped rather than reparented.
    let first_visible = tail.iter().position(|&c| cell(c) > 0).unwrap_or(tail.len());
    tail.drain(..first_visible);

    let mut out = head;
    out.push('…');
    out.extend(tail);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use norte_proto::Scheme;

    fn root() -> VPath {
        VPath::root(Scheme::new("mem").unwrap(), None)
    }

    /// `skip_cells` counts COLUMNS, not bytes or characters.
    #[test]
    fn skip_cells_counts_columns_and_does_not_split_a_width() {
        assert_eq!(skip_cells("hola", 0), "hola");
        assert_eq!(skip_cells("hola", 1), "ola");
        assert_eq!(skip_cells("hola", 4), "");
        assert_eq!(skip_cells("hola", 99), "", "overshooting is not an error");
    }

    /// **A wide character split by the cut leaves its gap blank.**
    ///
    /// Simply dropping it shifted that row one column relative to the ones
    /// next to it, and that breaks exactly what a horizontal scroll is for
    /// reading: a CSV, an aligned log. The grid is the feature.
    #[test]
    fn skip_cells_keeps_the_grid_when_it_splits_a_width() {
        // An ideogram is TWO columns: cutting at the first one takes it
        // whole, and leaves a space where its right half was.
        assert_eq!(skip_cells("漢字x", 1), " 字x");
        assert_eq!(skip_cells("漢字x", 2), "字x");
        assert_eq!(skip_cells("漢字x", 3), " x");
        assert_eq!(skip_cells("漢字x", 4), "x");

        // And what really matters: the CJK row and the ASCII one measure the
        // SAME after the same scroll, so their columns stay lined up.
        for n in 0..6 {
            assert_eq!(
                cells(&skip_cells("漢字x", n)),
                cells(&skip_cells("abcde", n)),
                "scrolled {n} columns, both rows measure the same"
            );
        }
    }

    /// A zero-width mark that would open the rest is DROPPED: it lost its
    /// base on the other side of the cut, and leaving it re-parents it to
    /// the next letter. It is the same thing `middle_ellipsis` and
    /// `ellipsis_at_bytes` do at the tail.
    #[test]
    fn skip_cells_does_not_leave_an_orphaned_mark_at_the_start() {
        assert_eq!(
            skip_cells("ae\u{301}b", 1),
            "e\u{301}b",
            "the `e` brings its own"
        );
        assert_eq!(skip_cells("ae\u{301}b", 2), "b", "the accent lost its `e`");

        // A ZWJ cluster cut in half would paint a DIFFERENT family than the
        // one in the file. With no leading ZWJ, they are separate people
        // —which is what is left— and not another invented family.
        let family = "👨\u{200D}👩\u{200D}👧\u{200D}👦";
        let rest = skip_cells(family, 2);
        assert!(
            !rest.starts_with('\u{200D}'),
            "never starts with a joiner: {rest:?}"
        );

        // And no row ever starts with something of zero width, for any
        // scroll offset.
        for n in 0..12 {
            let r = skip_cells(family, n);
            if let Some(c) = r.chars().next() {
                assert_ne!(
                    UnicodeWidthChar::width(c),
                    Some(0),
                    "n={n} starts at zero width: {r:?}"
                );
            }
        }
    }

    /// Encoding MEDIA-2: `path_display`'s TEXT must not contain ANY char
    /// from the `must_mask` set — the flag already came from `display_name`
    /// (broad criterion), but the text was `display_lossy`'s (Cc+bidi
    /// only): raw ZWSP/TAG painted identical invisible twins in the
    /// navigation popups, both badged. Corpus-driven: every canonical
    /// hostile name, as a real `VPath`'s segment.
    #[test]
    fn path_display_never_paints_maskable_chars() {
        // #98/F2 of the audit: the sweep ALSO iterates the whole
        // reinterpretation cycle — without this, `w1252_c1_controls` was
        // inert (with enc=None the bytes fall to U+FFFD BEFORE must_mask
        // sees the C1 controls windows-1252 does decode: U+009D is OSC).
        let encs = std::iter::once(None).chain(
            norte_encoding::name_reinterpret_cycle()
                .iter()
                .copied()
                .map(Some),
        );
        for enc in encs {
            for n in norte_testkit::corpus::hostile_names() {
                let p = root().join(norte_proto::Segment::new(n.bytes.clone()).unwrap());
                let (text, _) = path_display_with(&p, enc);
                assert!(
                    !text.chars().any(must_mask),
                    "{} under {enc:?}: no must_mask chars: {text:?}",
                    n.id
                );
            }
        }
    }

    /// #98 (fixture `utf8_accidental_cp866`): legacy bytes that are ALSO
    /// valid UTF-8 (Cyrillic "а") are NEVER reinterpreted — under any
    /// encoding of the cycle they come out intact and unbadged (the
    /// mixed-no-mojibake rule, indistinguishable with no metadata).
    #[test]
    fn accidental_utf8_is_not_reinterpreted_under_any_encoding() {
        let accidental = norte_testkit::corpus::hostile_names()
            .into_iter()
            .find(|n| n.id == "utf8_accidental_cp866")
            .expect("corpus fixture")
            .bytes;
        for enc in norte_encoding::name_reinterpret_cycle() {
            let (text, hostile) = display_name_with(&accidental, Some(*enc));
            assert_eq!(
                (text.as_str(), hostile),
                ("а", false),
                "{}: valid UTF-8 intact",
                enc.label()
            );
        }
    }

    /// #57: reinterpretation decodes ONLY non-UTF8 names (display), keeps
    /// the hostile badge, never touches a valid UTF-8 name, and hazard
    /// masking survives decoding (a cp437 that produces a control char is
    /// not painted raw).
    #[test]
    fn display_name_with_reinterprets_only_non_utf8_and_masks() {
        use norte_encoding::NameEncoding;
        // "CAFÉ.TXT" in cp437 (É = 0x90): decodes and FLAGS.
        let (text, hostile) = display_name_with(b"CAF\x90.TXT", Some(NameEncoding::Cp437));
        assert_eq!(text, "CAFÉ.TXT");
        assert!(hostile, "reinterpreted = painted differs from the bytes");
        // Valid UTF-8: intact even with reinterpretation active.
        let (text, hostile) = display_name_with("año.txt".as_bytes(), Some(NameEncoding::Cp437));
        assert_eq!(text, "año.txt");
        assert!(!hostile);
        // None = the usual display_name (lossy marked).
        assert_eq!(
            display_name_with(b"\xFF\xFE", None),
            display_name(b"\xFF\xFE")
        );
        // Post-decoding hazards: does IBM866 decode 0x1B… no — 0x1B is ASCII
        // (ESC passes through unchanged in cp437's lower half): it must come
        // out masked, never a raw ESC in the terminal.
        let (text, hostile) = display_name_with(b"\x1b]0;x\x90", Some(NameEncoding::Cp437));
        assert!(!text.contains('\u{1b}'), "ESC never raw: {text:?}");
        assert!(hostile);
    }

    /// `path_display`'s `⟨scheme authority⟩/` prefix EXACTLY mirrors
    /// `VPath::display_lossy`'s format (proto vpath.rs) for every scheme
    /// that gets announced: with clean segments both texts are identical.
    ///
    /// **With one exception, and it is pinned here**: `file` with no
    /// authority. They are two different surfaces —`display_lossy` is a
    /// LOG's, where there is no screen around it to say which machine is
    /// being talked about; this one is a row's, where the default scheme is
    /// noise on every line— and the test has to state what the difference
    /// is instead of letting it be discovered by reading the code.
    #[test]
    fn path_display_mirrors_display_lossys_prefix() {
        let clean = VPath::parse("sftp://oscar-host/docs/notas.txt").unwrap();
        assert_eq!(path_display(&clean).0, clean.display_lossy());
        let no_auth = VPath::parse("mem:///a/b").unwrap();
        assert_eq!(path_display(&no_auth).0, no_auth.display_lossy());
        let rt = root();
        assert_eq!(path_display(&rt).0, rt.display_lossy());

        // The local one diverges, and in a specific way: the same string
        // minus the label.
        let local = VPath::parse("file:///home/o/notas.txt").unwrap();
        assert_eq!(path_display(&local).0, "/home/o/notas.txt");
        assert_eq!(local.display_lossy(), "⟨file⟩/home/o/notas.txt");

        // And a `file` WITH authority IS announced: then it is another
        // machine.
        let remote = VPath::parse("file://otra/home/o").unwrap();
        assert_eq!(path_display(&remote).0, remote.display_lossy());
    }

    /// H3b encoding audit, FIX 3(b): the tail of a middle-truncated string
    /// must never BEGIN on a zero-width char.
    ///
    /// A combining mark is width 0, so the tail walker never breaks on one:
    /// when the base character it belongs to is the one that overflows the
    /// budget, the mark survives alone at the front of the tail and the
    /// terminal composes it onto the `…` — the accent migrates off its
    /// letter. The same shape leaves a ZWJ emoji cluster starting on its
    /// joiner. Corpus-driven: `nfd_e_acute` and `emoji_zwj_family` are the
    /// canonical fixtures for exactly this.
    #[test]
    fn middle_ellipsis_never_leaves_the_tail_starting_at_zero_width() {
        let corpus = norte_testkit::corpus::hostile_names();
        for id in ["nfd_e_acute", "emoji_zwj_family"] {
            let n = corpus
                .iter()
                .find(|n| n.id == id)
                .unwrap_or_else(|| panic!("{id} is in the canonical corpus"));
            let piece =
                String::from_utf8(n.bytes.clone()).unwrap_or_else(|_| panic!("{id} is UTF-8"));
            // Repeated: this way the cut falls at EVERY possible position
            // within and between clusters depending on the budget, without
            // hand-picking the `max` that reproduces the bug.
            let s = piece.repeat(8);
            for max in 1..=32 {
                let out = middle_ellipsis(&s, max);
                let Some(tail) = out.split('…').nth(1) else {
                    continue;
                };
                if let Some(c) = tail.chars().next() {
                    assert!(
                        UnicodeWidthChar::width(c).unwrap_or(0) > 0,
                        "[{id}] max={max}: the tail starts at U+{:04X} (width 0), \
                         which composes onto the `…`: {out:?}",
                        c as u32
                    );
                }
            }
        }
    }

    /// FIX 3(b) again, but over a display TITLE instead of a file name: the
    /// surface H3b introduces (help's sidebar, a page's rows) and that H3f
    /// will feed from plugin manifests.
    ///
    /// The canonical corpus had hostile names (bytes) and hostile chords (a
    /// codepoint), but nothing shaped like a title truncated in a narrow
    /// column: `hostile_titles` is that class, and `nfd_accent_on_the_cut`
    /// pins it here. An NFD accent weighs ZERO cells, so the tail walker
    /// never breaks on it; when the letter it belongs to is the one that
    /// overflows the budget, the accent survives ALONE at the front of the
    /// tail and the terminal composes it onto the `…` — the accent migrates
    /// from its letter to the ellipsis. The ZWJ cluster has the same shape.
    ///
    /// The three titles are swept across every width: which `max` reproduces
    /// the bug depends on the text, and hand-picking it pins today's bug
    /// instead of the invariant.
    #[test]
    fn middle_ellipsis_over_hostile_titles_does_not_orphan_marks() {
        for t in norte_testkit::corpus::hostile_titles() {
            for text in std::iter::once(t.text).chain(t.twin) {
                for max in 1..=40 {
                    let out = middle_ellipsis(text, max);
                    let Some(tail) = out.split('…').nth(1) else {
                        continue;
                    };
                    if let Some(c) = tail.chars().next() {
                        assert!(
                            UnicodeWidthChar::width(c).unwrap_or(0) > 0,
                            "[{}] max={max}: the tail starts at U+{:04X} (width \
                             0), which composes onto the `…`: {out:?}",
                            t.id,
                            c as u32
                        );
                    }
                    assert!(
                        out.chars()
                            .map(|c| UnicodeWidthChar::width(c).unwrap_or(0))
                            .sum::<usize>()
                            <= max,
                        "[{}] max={max}: the truncation overflows its budget: {out:?}",
                        t.id
                    );
                }
            }
        }
    }

    /// The other half of `truncation_twins`: the collision is REAL and
    /// cannot be prevented, so what is required is that the truncation be
    /// MARKED.
    ///
    /// Two different titles that share everything up to the cut paint
    /// identically in a narrow column — that is geometry, not a bug. What
    /// spec §6 does not allow is for the loss to be SILENT: the `…` is what
    /// tells the reader that what they see is not the whole title and that
    /// the row next to it might be something else.
    #[test]
    fn two_titles_that_collide_when_truncated_carry_a_mark() {
        let pair = norte_testkit::corpus::hostile_titles()
            .into_iter()
            .find(|t| t.id == "truncation_twins")
            .expect("corpus fixture");
        let twin = pair.twin.expect("a collision needs two strings");
        assert_ne!(pair.text, twin, "the fixture has to be a DIFFERENT pair");
        let a = middle_ellipsis(pair.text, 20);
        let b = middle_ellipsis(twin, 20);
        assert!(
            a.contains('…') && b.contains('…'),
            "the truncation is ALWAYS marked: {a:?} / {b:?}"
        );
        // And with no truncation, they do not collide: the collision is the
        // width's, not the data's.
        assert_ne!(
            middle_ellipsis(pair.text, 200),
            middle_ellipsis(twin, 200),
            "with room to spare the two titles are distinguishable"
        );
    }

    /// Byte truncation does not split a cluster: it neither leaves an
    /// orphaned combining mark stuck to the `…`, nor cuts an emoji family at
    /// its joiner. It is the same FIX 3(b) from the other end.
    #[test]
    fn ellipsis_at_bytes_leaves_no_orphaned_marks() {
        let corpus = norte_testkit::corpus::hostile_names();
        for id in ["nfd_e_acute", "emoji_zwj_family"] {
            let n = corpus
                .iter()
                .find(|n| n.id == id)
                .unwrap_or_else(|| panic!("{id} in the corpus"));
            let text = String::from_utf8_lossy(&n.bytes).into_owned();
            // A cap right INSIDE the cluster, tried at every possible byte.
            for cap in 4..=text.len() + 4 {
                let out = ellipsis_at_bytes(&text, cap);
                assert!(out.len() <= cap, "[{id}/{cap}] exceeds the cap");
                if out.ends_with('…') {
                    let without_mark = &out[..out.len() - '…'.len_utf8()];
                    if let Some(last) = without_mark.chars().next_back() {
                        assert!(
                            UnicodeWidthChar::width(last).unwrap_or(0) > 0,
                            "[{id}/{cap}] the ellipsis is left with an orphaned mark: {out:?}"
                        );
                    }
                }
            }
        }
    }

    /// What fits travels intact, and a cap that does not even leave room for
    /// the mark returns empty instead of an ellipsis that does not say what
    /// was cut.
    #[test]
    fn ellipsis_at_bytes_at_the_edges() {
        assert_eq!(ellipsis_at_bytes("hola", 4), "hola");
        assert_eq!(ellipsis_at_bytes("hola", 2), "");
        assert!(ellipsis_at_bytes("holaaa", 5).ends_with('…'));
    }

    /// `clean_utf8_path_over_clamp`'s premise (#277): a path of CLEAN UTF-8
    /// segments whose `path_display` goes past the bridge's cap.
    ///
    /// What is asserted here is what makes the fixture useful:
    /// `path_display` declares it FAITHFUL, because there is nothing to
    /// mask. Meaning that if the surface that paints it truncates it and
    /// does not say so, no flag is left to give the truncation away —and the
    /// ellipsis is a legal character in a name—. `display_expansion_over_clamp`
    /// is no use for this: its 0xFF bytes choose the lossy path and its flag
    /// already comes out `true` because of that.
    #[test]
    fn the_clean_path_over_the_cap_has_nothing_to_mask() {
        let segs = norte_testkit::corpus::clean_utf8_path_over_clamp();
        let mut p = VPath::parse("mem:///").expect("root");
        for s in &segs {
            p = p.join(norte_proto::Segment::new(s.clone()).expect("segment"));
        }
        let (text, hostile) = path_display(&p);
        assert!(
            !hostile,
            "the fixture exists for the TRUNCATION: if it already flags for another reason, it distinguishes nothing"
        );
        assert!(
            text.len() > 4096,
            "the path has to exceed MAX_STRING_BYTES: {}",
            text.len()
        );
    }

    /// A path line's `⟨scheme⟩/` prefix is a ROLE MARKER, and this is what
    /// pins it (#277).
    ///
    /// Three corpus names render, letter for letter, a line the host writes
    /// on its own: an approval's TTL in both languages and a batch report's
    /// verdict. All of them are ordinary printable text, so nothing gets
    /// masked and no flag trips; the only thing that stops a file from
    /// forging the host's sentence is that a PATH line is told apart from a
    /// BODY one when painted.
    ///
    /// Whoever removes that prefix as "noise" finds nothing red without
    /// this.
    #[test]
    fn a_path_line_cannot_forge_a_host_sentence() {
        let cases = [
            (
                "approval_ttl_line_spoof",
                norte_i18n::Lang::Es,
                "modal-approval-ttl",
            ),
            (
                "approval_ttl_line_spoof_en",
                norte_i18n::Lang::En,
                "modal-approval-ttl",
            ),
            (
                "journal_verdict_line_spoof",
                norte_i18n::Lang::Es,
                "modal-batch-stuck-journalled",
            ),
        ];
        let corpus = norte_testkit::corpus::hostile_names();
        for (id, lang, key) in cases {
            let n = corpus
                .iter()
                .find(|n| n.id == id)
                .unwrap_or_else(|| panic!("fixture {id} is in the corpus"));

            // The premise: the name IS the sentence, with not a single
            // difference.
            let sentence = if key == "modal-approval-ttl" {
                norte_i18n::ta_in(lang, key, &[("s", "3600")])
            } else {
                norte_i18n::t_in(lang, key)
            };
            let (name, hostile) = display_name(&n.bytes);
            assert_eq!(
                name, sentence,
                "[{id}] the fixture stopped being the sentence"
            );
            assert!(
                !hostile,
                "[{id}] there is nothing to mask: that is why the marker is needed"
            );

            // And the role marker is what disarms it. Over the THREE
            // schemes that paint differently, `file` included: this test
            // exists to catch whoever removes the prefix as noise, and
            // testing only `mem` did not catch exactly that — `file`'s
            // prefix was removed with the test green. What is asserted now
            // is the marker each one gets, not a literal string.
            for root_ in ["mem:///", "file:///", "sftp://h/"] {
                let p = VPath::parse(root_)
                    .expect("root")
                    .join(norte_proto::Segment::new(n.bytes.clone()).expect("segment"));
                let (line, _) = path_display(&p);
                assert_ne!(
                    line, sentence,
                    "[{id}/{root_}] a path was painted as a host sentence"
                );
                // `file` with no authority carries no `⟨…⟩`: its marker is
                // the leading `/`, which a name cannot carry (no supported
                // OS allows `/` in a segment). The others carry their own.
                if root_ == "file:///" {
                    assert!(
                        line.starts_with('/') && !line.starts_with("⟨"),
                        "[{id}] the local one paints with no label and with the \
                         slash in front: {line:?}"
                    );
                } else {
                    assert!(
                        line.starts_with('⟨'),
                        "[{id}/{root_}] the role marker disappeared: {line:?}"
                    );
                }
            }
        }
    }
}
