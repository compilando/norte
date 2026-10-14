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

/// The text of a run of pieces when every one of them was typed plain.
pub(crate) fn plain_text(pieces: &[Piece]) -> Option<String> {
    pieces
        .iter()
        .all(|p| p.plain)
        .then(|| String::from_utf8(bytes(pieces)).ok())
        .flatten()
}
