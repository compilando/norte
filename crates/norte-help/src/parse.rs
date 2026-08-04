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
    let mut out = Vec::new();
    let mut text = String::new();
    let mut rest = line;
    let mut closers = Closers {
        cmd: true,
        link: true,
    };
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
    out
}

/// What the scan has already ruled out, one flag per mark.
///
/// `rest` only ever shrinks, so a closer missing from one tail is missing
/// from every shorter tail: remembering that turns a line built out of
/// openers from quadratic into linear. Found by the long-line hardening
/// test, where 100k bytes of `{{cmd:` scanned the whole tail once per
/// opener and took seconds; a plugin `help.md` (task 6) is exactly where
/// such a line comes from.
struct Closers {
    /// A `}}` may still lie ahead.
    cmd: bool,
    /// A `]]` may still lie ahead.
    link: bool,
}

/// `{{cmd:…}}` and `[[…]]`, the corpus' two own marks.
fn take_mark(rest: &str, closers: &mut Closers) -> Option<(Span, usize)> {
    if closers.cmd
        && let Some(after) = rest.strip_prefix(CMD_OPEN)
    {
        let Some(end) = after.find(CMD_CLOSE) else {
            closers.cmd = false;
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
    fn a_hostile_corpus_line_neither_panics_nor_fabricates_a_command() {
        // Bidi overrides, zero-width joiners, tag characters and lone
        // invalid bytes, straight from the canonical corpus. Masking is NOT
        // this parser's job (task 6 masks BEFORE parsing spans), so the only
        // claims here are: it returns, and it invents nothing.
        let names = norte_testkit::corpus::hostile_names();
        assert!(!names.is_empty(), "the corpus must not be empty");
        let mut line = String::new();
        for n in &names {
            // The names are raw bytes; `parse` only ever sees `&str`, so the
            // lossy decode is exactly what the hostile path will feed it.
            line.push_str(&String::from_utf8_lossy(&n.bytes));
            line.push(' ');
        }
        let out = spans(&line);
        assert!(
            !out.iter().any(|s| matches!(s, Span::CommandRef(_))),
            "no hostile name may conjure a command reference"
        );

        // And each name on its own, so a failure names the culprit.
        for n in &names {
            let decoded = String::from_utf8_lossy(&n.bytes);
            let out = spans(&decoded);
            assert!(
                !out.iter().any(|s| matches!(s, Span::CommandRef(_))),
                "{} fabricated a command reference",
                n.id
            );
        }
    }

    #[test]
    fn a_long_line_with_unterminated_marks_returns_without_blowing_up() {
        // Two shapes of the same trap: one very long unterminated mark, and
        // many openers that each tempt the parser into rescanning the whole
        // tail (the quadratic shape). Before `Closers`, the second shape
        // took 10.7 s here in a debug build; it now takes ~0.1 s.
        let started = std::time::Instant::now();

        let one = format!("{CMD_OPEN}{}", "a".repeat(100_000));
        let out = spans(&one);
        assert_eq!(out.len(), 1, "one literal text span");
        assert!(matches!(out.first(), Some(Span::Text(_))));

        let many = CMD_OPEN.repeat(100_000 / CMD_OPEN.len());
        let out = spans(&many);
        assert!(
            !out.iter().any(|s| matches!(s, Span::CommandRef(_))),
            "no closer anywhere: nothing may be fabricated"
        );

        let links = LINK_OPEN.repeat(100_000 / LINK_OPEN.len());
        assert!(
            !spans(&links)
                .iter()
                .any(|s| matches!(s, Span::TopicLink(_))),
        );

        // A deliberately loose bound — two orders of magnitude above what
        // this costs today, and still an order below the quadratic version.
        // It is not a benchmark: it is the tripwire that tells a future
        // change it reintroduced the rescan.
        let elapsed = started.elapsed();
        assert!(
            elapsed < std::time::Duration::from_secs(5),
            "300k bytes of openers took {elapsed:?}: the scan went quadratic again"
        );
    }
}
