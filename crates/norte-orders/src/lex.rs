//! Words, pipes and the byte escape.
//!
//! Every byte of a word remembers whether it was typed PLAIN (unquoted,
//! unescaped). Only plain bytes can be syntax — `!`, `ext:`, `>`, `,` — so a
//! file literally called `other` or `!x` is written escaped and stays a name.

use std::ops::Range;

use crate::parse::{ParseError, ParseErrorKind};

/// One byte of a word.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Piece {
    pub byte: u8,
    /// Typed plain: may be syntax.
    pub plain: bool,
    /// Where its source starts in the line (the `\` of an escape).
    pub at: usize,
}

/// A lexed token.
#[derive(Debug, Clone)]
pub(crate) enum Tok {
    Word {
        span: Range<usize>,
        pieces: Vec<Piece>,
    },
    Pipe(usize),
}

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// The length of the Unicode space starting at byte `i`, or 0. Any
/// `White_Space` char separates words, not only ASCII ones: a no-break space
/// pasted after `>other` must not turn the keyword into a folder name.
fn space_at(line: &str, i: usize) -> usize {
    line.get(i..)
        .and_then(|rest| rest.chars().next())
        .filter(|c| c.is_whitespace())
        .map_or(0, char::len_utf8)
}

/// Lexes as far as the line allows: the tokens before an error survive, so
/// the box can still colour them.
pub(crate) fn lex(line: &str) -> (Vec<Tok>, Option<ParseError>) {
    let src = line.as_bytes();
    let mut toks = Vec::new();
    let mut i = 0;
    let err = |offset, kind| Some(ParseError { offset, kind });
    while i < src.len() {
        let b = src[i];
        let space = space_at(line, i);
        if space > 0 {
            i += space;
            continue;
        }
        if b == b'|' {
            toks.push(Tok::Pipe(i));
            i += 1;
            continue;
        }
        let start = i;
        let mut pieces = Vec::new();
        let mut quote: Option<usize> = None;
        while i < src.len() {
            let b = src[i];
            if quote.is_none() && (b == b'|' || space_at(line, i) > 0) {
                break;
            }
            match b {
                b'\'' => {
                    quote = if quote.is_some() { None } else { Some(i) };
                    i += 1;
                }
                b'\\' => {
                    let at = i;
                    let byte = match src.get(i + 1) {
                        Some(b'x') => {
                            let hi = src.get(i + 2).copied().and_then(hex);
                            let lo = src.get(i + 3).copied().and_then(hex);
                            let (Some(hi), Some(lo)) = (hi, lo) else {
                                return (toks, err(at, ParseErrorKind::BadEscape));
                            };
                            i += 4;
                            hi << 4 | lo
                        }
                        Some(&c @ (b'\\' | b' ' | b'|' | b'\'')) => {
                            i += 2;
                            c
                        }
                        _ => return (toks, err(at, ParseErrorKind::BadEscape)),
                    };
                    pieces.push(Piece {
                        byte,
                        plain: false,
                        at,
                    });
                }
                _ => {
                    pieces.push(Piece {
                        byte: b,
                        plain: quote.is_none(),
                        at: i,
                    });
                    i += 1;
                }
            }
        }
        if let Some(q) = quote {
            return (toks, err(q, ParseErrorKind::Unterminated));
        }
        toks.push(Tok::Word {
            span: start..i,
            pieces,
        });
    }
    (toks, None)
}

/// The bytes of a run of pieces.
pub(crate) fn bytes(pieces: &[Piece]) -> Vec<u8> {
    pieces.iter().map(|p| p.byte).collect()
}

/// Whether `pieces` starts with `prefix` typed plain.
pub(crate) fn plain_prefix(pieces: &[Piece], prefix: &[u8]) -> bool {
    pieces.len() >= prefix.len()
        && pieces
            .iter()
            .zip(prefix)
            .all(|(p, b)| p.plain && p.byte == *b)
}

/// Whether `pieces` is exactly `word`, typed plain.
pub(crate) fn plain_eq(pieces: &[Piece], word: &[u8]) -> bool {
    pieces.len() == word.len() && plain_prefix(pieces, word)
}

/// Unicode `Cf` (format characters, Unicode 16): drawn as nothing, so glued
/// to a keyword they make a name that looks exactly like it. A table of our
/// own because this crate links no Unicode property crate but
/// `unicode-normalization`, which does not expose the general category.
const FORMAT_CHARS: &[(char, char)] = &[
    ('\u{00AD}', '\u{00AD}'),
    ('\u{0600}', '\u{0605}'),
    ('\u{061C}', '\u{061C}'),
    ('\u{06DD}', '\u{06DD}'),
    ('\u{070F}', '\u{070F}'),
    ('\u{0890}', '\u{0891}'),
    ('\u{08E2}', '\u{08E2}'),
    ('\u{180E}', '\u{180E}'),
    ('\u{200B}', '\u{200F}'),
    ('\u{202A}', '\u{202E}'),
    ('\u{2060}', '\u{2064}'),
    ('\u{2066}', '\u{206F}'),
    ('\u{FEFF}', '\u{FEFF}'),
    ('\u{FFF9}', '\u{FFFB}'),
    ('\u{110BD}', '\u{110BD}'),
    ('\u{110CD}', '\u{110CD}'),
    ('\u{13430}', '\u{1343F}'),
    ('\u{1BCA0}', '\u{1BCA3}'),
    ('\u{1D173}', '\u{1D17A}'),
    ('\u{E0001}', '\u{E0001}'),
    ('\u{E0020}', '\u{E007F}'),
];

/// Drawn as nothing: `Cf`, plus the invisible letters and marks the printer
/// already escapes (Hangul fillers, CGJ, variation selectors).
fn is_format(c: char) -> bool {
    FORMAT_CHARS.iter().any(|(lo, hi)| (*lo..=*hi).contains(&c))
        || crate::print::INVISIBLE_LETTERS.contains(&c)
        || crate::print::INVISIBLE_MARKS
            .iter()
            .any(|(lo, hi)| (*lo..=*hi).contains(&c))
}

/// The plain-typed format char that `pieces` starts with, as its piece count.
fn format_len(pieces: &[Piece]) -> usize {
    let n = match pieces.first().map(|p| p.byte) {
        Some(0xC0..=0xDF) => 2,
        Some(0xE0..=0xEF) => 3,
        Some(0xF0..=0xF7) => 4,
        _ => return 0,
    };
    let Some(run) = pieces.get(..n) else {
        return 0;
    };
    let c = std::str::from_utf8(&bytes(run))
        .ok()
        .and_then(|s| s.chars().next());
    if run.iter().all(|p| p.plain) && c.is_some_and(is_format) {
        n
    } else {
        0
    }
}

/// Whether `run` is a variation selector or the zero-width joiner: the
/// invisible characters an emoji is made of.
fn joins_emoji(run: &[Piece]) -> bool {
    std::str::from_utf8(&bytes(run))
        .ok()
        .and_then(|s| s.chars().next())
        .is_some_and(
            |c| matches!(c, '\u{200D}' | '\u{FE00}'..='\u{FE0F}' | '\u{E0100}'..='\u{E01EF}'),
        )
}

/// `pieces` without any invisible character typed plain, and the line
/// offset of the first one dropped; `None` when there were none. Escaped or
/// quoted ones are kept: they say "this is a name" on purpose.
pub(crate) fn strip_format(pieces: &[Piece]) -> Option<(Vec<Piece>, usize)> {
    let mut kept = Vec::with_capacity(pieces.len());
    let mut first = None;
    let mut i = 0;
    while i < pieces.len() {
        // A continuation byte is never a lead, so a visible multi-byte char
        // is kept whole, one piece at a time.
        let mut n = format_len(&pieces[i..]);
        // A variation selector or a ZWJ right after a non-ASCII character is
        // part of what was typed — `❤️`, a family emoji — not a disguise:
        // every keyword is ASCII, so `other` + ZWJ is still caught.
        if n > 0
            && kept.last().is_some_and(|p: &Piece| p.byte >= 0x80)
            && joins_emoji(&pieces[i..i + n])
        {
            n = 0;
        }
        if n == 0 {
            kept.push(pieces[i]);
            i += 1;
        } else {
            first.get_or_insert(pieces[i].at);
            i += n;
        }
    }
    first.map(|at| (kept, at))
}

/// The text of a run of pieces when every one of them was typed plain.
pub(crate) fn plain_text(pieces: &[Piece]) -> Option<String> {
    pieces
        .iter()
        .all(|p| p.plain)
        .then(|| String::from_utf8(bytes(pieces)).ok())
        .flatten()
}
