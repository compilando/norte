//! Markdown-lite parser (ADR 0040, decision 3): a CLOSED vocabulary. What
//! this parser cannot express, a third-party `help.md` cannot paint on the
//! terminal — that is the security barrier, not an allowlist bolted on
//! afterwards.

use crate::model::{Span, TopicId};

/// Opener of a command reference. Opener and closer are constants so that
/// the needle we search for and the number of bytes we skip can never
/// desynchronise: every offset below is derived from these literals.
const CMD_OPEN: &str = "{{cmd:";

/// Closer of a command reference.
const CMD_CLOSE: &str = "}}";

/// Opener of a topic link.
const LINK_OPEN: &str = "[[";

/// Closer of a topic link.
const LINK_CLOSE: &str = "]]";

/// Cuts a line into [`Span`]s. A badly closed mark stays literal text: it
/// never produces a reference to a command nobody wrote.
// Narrowest possible suppression: the module is private and nothing outside
// its own tests calls `spans` yet — the block parser will, in task 5 of this
// phase. It sits on `spans` ALONE because rustc treats a lint-allowed item
// as a live root, so everything `spans` reaches (`take_mark`, `take_delim`,
// the four constants) is analysed honestly and would be reported if it
// really went unused. `expect` and not `allow` so that the day task 5 calls
// `spans` the expectation goes unfulfilled and the compiler forces this line
// out; task 5 deletes it.
#[cfg_attr(not(test), expect(dead_code))]
fn spans(line: &str) -> Vec<Span> {
    spans_counted(line).0
}

/// [`spans`], plus how many tail scans came back empty.
///
/// The count is the observable form of the [`Closers`] invariant: it is
/// bounded by one per mark kind, whatever the input. Tests assert on it
/// instead of on a stopwatch, because a wall clock measures the machine and
/// this measures the algorithm.
fn spans_counted(line: &str) -> (Vec<Span>, usize) {
    let mut out = Vec::new();
    let mut text = String::new();
    let mut rest = line;
    let mut closers = Closers::new();
    while !rest.is_empty() {
        let taken = take_mark(rest, &mut closers).or_else(|| take_delim(rest));
        if let Some((span, len)) = taken {
            if !text.is_empty() {
                out.push(Span::Text(std::mem::take(&mut text)));
            }
            out.push(span);
            rest = &rest[len..];
            continue;
        }
        let ch = rest.chars().next().unwrap_or_default();
        text.push(ch);
        rest = &rest[ch.len_utf8()..];
    }
    if !text.is_empty() {
        out.push(Span::Text(text));
    }
    (out, closers.failed_tail_scans)
}

/// What the scan has already ruled out, one flag per mark.
///
/// `rest` only ever shrinks, so a closer missing from one tail is missing
/// from every shorter tail: remembering that turns a line built out of
/// openers from quadratic into linear. Found by the long-line hardening
/// test, where 100k bytes of `{{cmd:` scanned the whole tail once per
/// opener; a plugin `help.md` (task 6) is exactly where such a line comes
/// from.
///
/// INVARIANT: one instance per call, consumed against a strictly shrinking
/// suffix of ONE input. That is the whole basis of the memo — "no closer in
/// this tail" only implies "no closer in any later tail" while later tails
/// are suffixes of this one. Sharing an instance across inputs (hoisting it
/// out of a per-line loop, say) is UNSOUND: line 2 is not a suffix of line
/// 1, so every mark after the first unclosed one would be silently swallowed
/// and no test on line 1 would notice. Hence the private [`Closers::new`]
/// and no `Clone`, `Copy` or `Default`: an instance is only ever born inside
/// [`spans_counted`].
struct Closers {
    /// A `}}` may still lie ahead.
    cmd: bool,
    /// A `]]` may still lie ahead.
    link: bool,
    /// Tail scans that found no closer. Bounded by one per mark kind while
    /// the invariant above holds; unbounded the moment the memo is removed.
    failed_tail_scans: usize,
}

impl Closers {
    /// A fresh memo for ONE input. See the type's INVARIANT.
    fn new() -> Self {
        Self {
            cmd: true,
            link: true,
            failed_tail_scans: 0,
        }
    }
}

/// `{{cmd:…}}` and `[[…]]`, the corpus' two own marks.
fn take_mark(rest: &str, closers: &mut Closers) -> Option<(Span, usize)> {
    if closers.cmd
        && let Some(after) = rest.strip_prefix(CMD_OPEN)
    {
        let Some(end) = after.find(CMD_CLOSE) else {
            closers.cmd = false;
            closers.failed_tail_scans += 1;
            return None;
        };
        let id = after[..end].trim();
        if id.is_empty() {
            return None;
        }
        return Some((
            Span::CommandRef(id.to_owned()),
            CMD_OPEN.len() + end + CMD_CLOSE.len(),
        ));
    }
    if closers.link
        && let Some(after) = rest.strip_prefix(LINK_OPEN)
    {
        let Some(end) = after.find(LINK_CLOSE) else {
            closers.link = false;
            closers.failed_tail_scans += 1;
            return None;
        };
        let id = after[..end].trim();
        if id.is_empty() {
            return None;
        }
        return Some((
            Span::TopicLink(TopicId::new(id)),
            LINK_OPEN.len() + end + LINK_CLOSE.len(),
        ));
    }
    None
}

/// `**strong**`, `*emphasis*` and `` `code` ``. Inline code is taken first
/// and its content is NOT reinterpreted.
fn take_delim(rest: &str) -> Option<(Span, usize)> {
    for (open, close, build) in [
        (
            "`",
            "`",
            (|s: &str| Span::Code(s.to_owned())) as fn(&str) -> Span,
        ),
        ("**", "**", |s: &str| Span::Strong(s.to_owned())),
        ("*", "*", |s: &str| Span::Emph(s.to_owned())),
    ] {
        if let Some(after) = rest.strip_prefix(open)
            && let Some(end) = after.find(close)
            && end > 0
        {
            return Some((build(&after[..end]), open.len() + end + close.len()));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Span, TopicId};

    #[test]
    fn plain_text_is_a_single_span() {
        assert_eq!(
            spans("plain text"),
            vec![Span::Text("plain text".to_owned())]
        );
    }

    #[test]
    fn recognises_the_four_inline_marks() {
        assert_eq!(
            spans("a **b** c *d* e `f` g"),
            vec![
                Span::Text("a ".to_owned()),
                Span::Strong("b".to_owned()),
                Span::Text(" c ".to_owned()),
                Span::Emph("d".to_owned()),
                Span::Text(" e ".to_owned()),
                Span::Code("f".to_owned()),
                Span::Text(" g".to_owned()),
            ]
        );
    }

    #[test]
    fn recognises_the_live_marks_without_resolving_them() {
        assert_eq!(
            spans("press {{cmd:fs.copy}} then see [[selection]]"),
            vec![
                Span::Text("press ".to_owned()),
                Span::CommandRef("fs.copy".to_owned()),
                Span::Text(" then see ".to_owned()),
                Span::TopicLink(TopicId::new("selection")),
            ]
        );
    }

    #[test]
    fn an_unclosed_mark_stays_literal_text() {
        assert_eq!(
            spans("{{cmd:fs.copy oops"),
            vec![Span::Text("{{cmd:fs.copy oops".to_owned())],
            "a broken mark must never invent a phantom command"
        );
    }

    #[test]
    fn inline_code_does_not_interpret_marks() {
        assert_eq!(
            spans("`{{cmd:x}}`"),
            vec![Span::Code("{{cmd:x}}".to_owned())]
        );
    }

    // --- Hardening: this parser sits on the path of third-party plugin help
    // (task 6), so hostile input must DEGRADE into literal text — never
    // panic, never cut a multi-byte character in half, and above all never
    // fabricate a `CommandRef`, which the corpus checks of task 8 read as a
    // claim that the command exists.

    #[test]
    fn multibyte_content_survives_byte_for_byte_inside_every_mark() {
        // Every offset in the parser is a BYTE offset: a mark whose content
        // abuts the closer with a multi-byte character is where an
        // off-by-one panics or truncates.
        assert_eq!(
            spans("{{cmd:ñ.copy}}"),
            vec![Span::CommandRef("ñ.copy".to_owned())]
        );
        assert_eq!(
            spans("[[ñ]]"),
            vec![Span::TopicLink(TopicId::new("ñ"))],
            "the id keeps its bytes: the model does not normalise"
        );
        assert_eq!(spans("**ñ**"), vec![Span::Strong("ñ".to_owned())]);
        assert_eq!(spans("`ñ`"), vec![Span::Code("ñ".to_owned())]);
    }

    #[test]
    fn a_line_of_emoji_and_cjk_never_panics() {
        let line = "👨‍👩‍👧 日本語 — ñ 🇪🇸";
        assert_eq!(
            spans(line),
            vec![Span::Text(line.to_owned())],
            "nothing to mark up: it comes back whole, byte for byte"
        );
    }

    #[test]
    fn an_empty_id_stays_literal_text() {
        assert_eq!(
            spans("{{cmd:}}"),
            vec![Span::Text("{{cmd:}}".to_owned())],
            "an empty CommandRef would claim a command with no name exists"
        );
        assert_eq!(spans("[[]]"), vec![Span::Text("[[]]".to_owned())]);
    }

    #[test]
    fn a_whitespace_only_id_stays_literal_text() {
        assert_eq!(
            spans("{{cmd:   }}"),
            vec![Span::Text("{{cmd:   }}".to_owned())],
            "trimming to nothing is still nothing"
        );
        assert_eq!(spans("[[   ]]"), vec![Span::Text("[[   ]]".to_owned())]);
    }

    #[test]
    fn unmatched_emphasis_delimiters_stay_literal() {
        assert_eq!(spans("a ** b"), vec![Span::Text("a ** b".to_owned())]);
        assert_eq!(spans("a * b"), vec![Span::Text("a * b".to_owned())]);
        assert_eq!(spans("**"), vec![Span::Text("**".to_owned())]);
        assert_eq!(spans("*"), vec![Span::Text("*".to_owned())]);
        assert_eq!(
            spans("a `b"),
            vec![Span::Text("a `b".to_owned())],
            "an unclosed backtick is a backtick, not a code span to the end"
        );
    }

    #[test]
    fn a_mark_that_looks_nested_yields_exactly_one_span() {
        // The inner `[[b]]` is content of the command reference, not a
        // second mark: emitting a topic link here would invent a jump the
        // author never wrote.
        assert_eq!(
            spans("{{cmd:a[[b]]c}}"),
            vec![Span::CommandRef("a[[b]]c".to_owned())]
        );
    }

    #[test]
    fn the_reverse_nesting_also_yields_exactly_one_span() {
        // The other direction, where a topic id could swallow a command
        // mark: the `{{cmd:b}}` inside is content of the link, and the line
        // must NOT produce a command reference nobody wrote.
        let out = spans("[[a{{cmd:b}}c]]");
        assert_eq!(
            out,
            vec![Span::TopicLink(TopicId::new("a{{cmd:b}}c"))],
            "the id keeps the inner mark as literal bytes"
        );
        assert!(!out.iter().any(|s| matches!(s, Span::CommandRef(_))));
    }

    #[test]
    fn a_padded_id_is_trimmed_and_only_at_the_edges() {
        // This `trim` is the ONE place the parser normalises an id, and it
        // deliberately contradicts `TopicId`'s documented contract that ids
        // are never normalised. Pinned on purpose, on BOTH sides: a change
        // to `trim_start()` (or to no trim at all) must fail here rather
        // than silently split one topic into two ids that render the same.
        assert_eq!(
            spans("{{cmd: fs.copy }}"),
            vec![Span::CommandRef("fs.copy".to_owned())]
        );
        assert_eq!(
            spans("[[ selection ]]"),
            vec![Span::TopicLink(TopicId::new("selection"))]
        );
        // Only at the edges: inner whitespace is part of the id.
        assert_eq!(
            spans("{{cmd: fs copy }}"),
            vec![Span::CommandRef("fs copy".to_owned())]
        );
        assert_eq!(
            spans("[[ a b ]]"),
            vec![Span::TopicLink(TopicId::new("a b"))]
        );
        // Delimiters do NOT trim: `**  x  **` is emphasis over the spaces.
        assert_eq!(spans("** x **"), vec![Span::Strong(" x ".to_owned())]);
        assert_eq!(spans("` x `"), vec![Span::Code(" x ".to_owned())]);
    }

    /// The bytes each span carries, in order: no syntax, just what a
    /// renderer would put on screen (or, for the live marks, the id it would
    /// resolve).
    fn payloads(spans: &[Span]) -> Vec<&str> {
        spans
            .iter()
            .map(|s| match s {
                Span::Text(t) | Span::Strong(t) | Span::Emph(t) | Span::Code(t) => t.as_str(),
                Span::CommandRef(id) => id.as_str(),
                Span::TopicLink(id) => id.as_str(),
            })
            .collect()
    }

    /// The lossy decode of every hostile name in the canonical corpus. The
    /// names are raw bytes and the parser only ever sees `&str`, so the
    /// lossy decode is exactly what the hostile path (task 6) feeds it.
    fn hostile_lines() -> Vec<(String, String)> {
        norte_testkit::corpus::hostile_names()
            .into_iter()
            .map(|n| (n.id, String::from_utf8_lossy(&n.bytes).into_owned()))
            .collect()
    }

    /// Does this text carry any of the syntax the parser reacts to?
    fn has_mark_syntax(s: &str) -> bool {
        s.contains(['{', '[', '*', '`'])
    }

    #[test]
    fn a_mark_free_hostile_line_round_trips_byte_for_byte() {
        // Not panicking is the cheap half of the claim; the other half is
        // that nothing is DROPPED. Bidi overrides, zero-width joiners, tag
        // characters and lone invalid bytes must come back out whole —
        // masking is NOT this parser's job (task 6 masks BEFORE span
        // parsing), so any byte that goes missing here went missing for good.
        let names = hostile_lines();
        assert!(names.len() >= 32, "the canonical corpus must not shrink");

        let mut joined = String::new();
        for (id, text) in &names {
            if has_mark_syntax(text) {
                continue;
            }
            assert_eq!(
                payloads(&spans(text)).concat(),
                *text,
                "[{id}] lost or altered bytes"
            );
            joined.push_str(text);
            joined.push(' ');
        }
        assert!(!joined.is_empty(), "the corpus must not be empty");
        assert_eq!(
            payloads(&spans(&joined)).concat(),
            joined,
            "the whole line comes back byte for byte"
        );
    }

    #[test]
    fn hostile_names_spliced_into_marks_neither_fabricate_nor_mangle() {
        // Bare hostile names carry no mark syntax, so they can only prove
        // the parser survives them. Splicing each one INTO a mark is what
        // gives the no-fabrication claim something to bite on.
        for (id, text) in hostile_lines() {
            for (open, close) in [
                (CMD_OPEN, CMD_CLOSE),
                (LINK_OPEN, LINK_CLOSE),
                ("`", "`"),
                ("**", "**"),
            ] {
                let line = format!("{open}{text}{close}");
                let out = spans(&line);
                for p in payloads(&out) {
                    assert!(
                        line.contains(p),
                        "[{id}] in {open}…{close}: payload {p:?} is not in the input — \
                         the parser invented or mangled bytes"
                    );
                }
                // The exact shape, whenever the splice cannot end the mark
                // early: one span, payload byte-identical to the name.
                if !text.contains(close) && !text.trim().is_empty() {
                    let trimmed = if open == CMD_OPEN || open == LINK_OPEN {
                        text.trim()
                    } else {
                        text.as_str()
                    };
                    assert_eq!(
                        payloads(&out),
                        vec![trimmed],
                        "[{id}] in {open}…{close}: expected one span with the name verbatim"
                    );
                }
            }
        }
    }

    #[test]
    fn the_corpus_bidi_command_mark_keeps_its_payload_byte_for_byte() {
        // `cmd_mark_bidi_payload`: a whole `{{cmd:…}}` whose id carries a
        // RIGHT-TO-LEFT OVERRIDE. The parser must hand the id over with its
        // bytes intact and UNRESOLVED — it neither masks (task 6 does, on
        // the way in) nor validates (task 8's corpus check does, byte-exact,
        // which is precisely why the spoofed id will match no real command).
        let (_, text) = hostile_lines()
            .into_iter()
            .find(|(id, _)| id == "cmd_mark_bidi_payload")
            .expect("the fixture lives in the canonical corpus");

        let out = spans(&text);
        assert_eq!(out, vec![Span::CommandRef("\u{202e}fs.copy".to_owned())]);
        let Some(Span::CommandRef(cmd)) = out.first() else {
            panic!("a command reference");
        };
        assert_eq!(
            cmd.as_bytes(),
            "\u{202e}fs.copy".as_bytes(),
            "the override is carried, not stripped and not normalised"
        );
        assert_ne!(cmd, "fs.copy", "it must never pass for the real command");
    }

    #[test]
    fn a_long_line_with_unterminated_marks_scans_each_tail_once_per_mark() {
        // Two shapes of the same trap: one very long unterminated mark, and
        // many openers that each tempt the parser into rescanning the whole
        // tail (the quadratic shape). The claim is deterministic and about
        // the algorithm, not the machine: a tail scan that finds no closer
        // may happen at most ONCE per mark kind, because `Closers` remembers
        // it. Delete the memo and this count becomes the number of openers.
        let one = format!("{CMD_OPEN}{}", "a".repeat(100_000));
        let (out, scans) = spans_counted(&one);
        assert_eq!(out.len(), 1, "one literal text span");
        assert!(matches!(out.first(), Some(Span::Text(_))));
        assert_eq!(scans, 1, "a single fruitless scan of the tail");

        let many = CMD_OPEN.repeat(100_000 / CMD_OPEN.len());
        let (out, scans) = spans_counted(&many);
        assert!(
            !out.iter().any(|s| matches!(s, Span::CommandRef(_))),
            "no closer anywhere: nothing may be fabricated"
        );
        assert_eq!(scans, 1, "~16k openers, ONE scan");

        let links = LINK_OPEN.repeat(100_000 / LINK_OPEN.len());
        let (out, scans) = spans_counted(&links);
        assert!(!out.iter().any(|s| matches!(s, Span::TopicLink(_))));
        assert_eq!(scans, 1);

        // Both kinds in one line: one scan each, and they are independent.
        let (out, scans) = spans_counted(&format!("{many}{links}"));
        assert!(
            !out.iter()
                .any(|s| matches!(s, Span::CommandRef(_) | Span::TopicLink(_))),
            "still nothing fabricated"
        );
        assert_eq!(scans, 2, "one per mark kind, whatever the length");
    }
}
