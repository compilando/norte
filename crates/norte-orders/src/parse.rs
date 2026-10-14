//! From a line to an [`Order`], or to the byte where it stops making sense.

use std::ops::Range;

use crate::ast::{Cmp, Dest, KindSel, Order, Pred, Query, Stage, Term};
use crate::lex::{Piece, Tok, bytes, lex, plain_eq, plain_prefix, plain_text};

/// Why a line is not an order, and where.
///
/// ```
/// let e = norte_orders::parse("sel | cp").unwrap_err();
/// assert_eq!(e.kind, norte_orders::ParseErrorKind::MissingArg);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{kind} at byte {offset}")]
pub struct ParseError {
    /// Byte offset in the line.
    pub offset: usize,
    /// What went wrong.
    pub kind: ParseErrorKind,
}

/// The kinds of [`ParseError`].
///
/// ```
/// use norte_orders::{parse, ParseErrorKind};
/// assert_eq!(parse("redo").unwrap_err().kind, ParseErrorKind::NotYet);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ParseErrorKind {
    /// Nothing to run (an empty line, or an empty stage around a `|`).
    #[error("empty order")]
    Empty,
    /// The first word of a stage is not a verb nor a catalogue id.
    #[error("unknown verb")]
    UnknownVerb,
    /// `redo`: the journal has no redo yet.
    #[error("not yet")]
    NotYet,
    /// `log`/`undo` piped to or from something.
    #[error("stands alone")]
    StandsAlone,
    /// A term or argument that means nothing here.
    #[error("bad term")]
    BadTerm,
    /// A number or unit that does not parse, or overflows.
    #[error("bad number")]
    BadNumber,
    /// `\x` without two hex digits, an unknown escape, or `\` at the end.
    #[error("bad escape")]
    BadEscape,
    /// `cp`/`mv` without a `>` destination.
    #[error("missing destination")]
    MissingDest,
    /// A verb without its argument.
    #[error("missing argument")]
    MissingArg,
    /// A `'` never closed.
    #[error("unterminated quote")]
    Unterminated,
}

/// What a span of the line is, for colouring.
///
/// ```
/// use norte_orders::{tokens, TokenClass};
/// assert_eq!(tokens("trash")[0].1, TokenClass::Verb);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenClass {
    /// A verb or catalogue id.
    Verb,
    /// A query key (`ext:`, `size>`, a leading `!`).
    Key,
    /// A key's value, a bare glob, or a verb's argument.
    Value,
    /// `|`.
    Pipe,
    /// `>other`, `>/path`.
    Dest,
    /// From the error's offset to the end of its word, and everything after.
    Error,
}

type Spans = Vec<(Range<usize>, TokenClass)>;

/// Parses a line.
///
/// ```
/// use norte_orders::{parse, Dest, Stage};
/// let o = parse("sel *.log | mv >/tmp/old").expect("parse");
/// assert_eq!(o.stages[1], Stage::Mv(Dest::Path(b"/tmp/old".to_vec())));
/// ```
///
/// # Errors
/// [`ParseError`] with the byte offset where the line stops being an order.
pub fn parse(line: &str) -> Result<Order, ParseError> {
    run(line, &mut Vec::new())
}

/// Colour spans over `line`, in order. Past a parse error everything is
/// [`TokenClass::Error`], from the error's offset on.
///
/// ```
/// use norte_orders::{tokens, TokenClass};
/// let t = tokens("sel *.rs | cp >other");
/// assert_eq!(t.last().map(|s| s.1), Some(TokenClass::Dest));
/// ```
#[must_use]
pub fn tokens(line: &str) -> Vec<(Range<usize>, TokenClass)> {
    let mut spans = Vec::new();
    let mut lexed: Vec<Range<usize>> = Vec::new();
    let (toks, _) = lex(line);
    for t in &toks {
        match t {
            Tok::Word { span, .. } => lexed.push(span.clone()),
            Tok::Pipe(at) => lexed.push(*at..*at + 1),
        }
    }
    if let Err(e) = run(line, &mut spans) {
        spans.retain(|(r, _)| r.end <= e.offset);
        // The word the error falls in, from the error on; then the rest.
        let word_end = lexed
            .iter()
            .find(|r| r.contains(&e.offset))
            .map_or(line.len(), |r| r.end);
        let end = if e.offset < word_end {
            word_end
        } else {
            line.len()
        };
        spans.push((e.offset..end.max(e.offset), TokenClass::Error));
        for r in lexed.iter().filter(|r| r.start >= end) {
            spans.push((r.clone(), TokenClass::Error));
        }
    }
    spans
}

fn fail<T>(offset: usize, kind: ParseErrorKind) -> Result<T, ParseError> {
    Err(ParseError { offset, kind })
}

fn run(line: &str, spans: &mut Spans) -> Result<Order, ParseError> {
    let (toks, lex_err) = lex(line);
    // Stages split on pipes; an error past the last good token still lets the
    // stages before it colour.
    let mut segments: Vec<Vec<(Range<usize>, Vec<Piece>)>> = vec![Vec::new()];
    let mut pipes = Vec::new();
    for t in toks {
        match t {
            Tok::Pipe(at) => {
                pipes.push(at);
                segments.push(Vec::new());
            }
            Tok::Word { span, pieces } => {
                if let Some(seg) = segments.last_mut() {
                    seg.push((span, pieces));
                }
            }
        }
    }
    let mut stages = Vec::new();
    let n = segments.len();
    for (k, seg) in segments.iter().enumerate() {
        if k > 0
            && let Some(&at) = pipes.get(k - 1)
        {
            spans.push((at..at + 1, TokenClass::Pipe));
        }
        if seg.is_empty() {
            if let Some(e) = lex_err {
                return Err(e);
            }
            let at = if k == 0 {
                pipes.first().copied().unwrap_or(line.len())
            } else {
                pipes.get(k - 1).map_or(line.len(), |p| p + 1)
            };
            return fail(at, ParseErrorKind::Empty);
        }
        let is_last = k + 1 == n;
        let end = seg.last().map_or(line.len(), |(r, _)| r.end);
        match stage(seg, end, spans) {
            Ok(s) => stages.push(s),
            // A lex error inside the last stage explains it better.
            Err(e) => return Err(lex_err.filter(|_| is_last).unwrap_or(e)),
        }
    }
    if let Some(e) = lex_err {
        return Err(e);
    }
    if stages.len() > 1
        && let Some(k) = stages
            .iter()
            .position(|s| matches!(s, Stage::Log(_) | Stage::Undo(_)))
    {
        let at = segments
            .get(k)
            .and_then(|s| s.first())
            .map_or(0, |(r, _)| r.start);
        return fail(at, ParseErrorKind::StandsAlone);
    }
    Ok(Order { stages })
}

fn stage(
    seg: &[(Range<usize>, Vec<Piece>)],
    end: usize,
    spans: &mut Spans,
) -> Result<Stage, ParseError> {
    let (vspan, vpieces) = &seg[0];
    let args = &seg[1..];
    let Some(verb) = plain_text(vpieces) else {
        return fail(vspan.start, ParseErrorKind::UnknownVerb);
    };
    let no_args = || match args.first() {
        Some((r, _)) => fail(r.start, ParseErrorKind::BadTerm),
        None => Ok(()),
    };
    let known = matches!(
        verb.as_str(),
        "sel" | "cp" | "mv" | "trash" | "mkdir" | "cd" | "log" | "undo"
    ) || is_catalogue_id(&verb);
    if known {
        spans.push((vspan.clone(), TokenClass::Verb));
    }
    match verb.as_str() {
        "sel" => {
            if args.is_empty() {
                return fail(end, ParseErrorKind::MissingArg);
            }
            let mut terms = Vec::new();
            for (r, p) in args {
                terms.push(term(r, p, spans)?);
            }
            Ok(Stage::Sel(Query { terms }))
        }
        "cp" => dest(args, end, spans).map(Stage::Cp),
        "mv" => dest(args, end, spans).map(Stage::Mv),
        "trash" => no_args().map(|()| Stage::Trash),
        "mkdir" => one_arg(args, end, spans).map(Stage::Mkdir),
        "cd" => one_arg(args, end, spans).map(Stage::Cd),
        "log" => count(args, spans).map(Stage::Log),
        "undo" => count(args, spans).map(Stage::Undo),
        "redo" => fail(vspan.start, ParseErrorKind::NotYet),
        id if is_catalogue_id(id) => no_args().map(|()| Stage::Catalogue(verb)),
        _ => fail(vspan.start, ParseErrorKind::UnknownVerb),
    }
}

/// A catalogue id: lowercase words joined by dots, e.g. `pane.swap`.
fn is_catalogue_id(s: &str) -> bool {
    s.contains('.')
        && !s.starts_with('.')
        && !s.ends_with('.')
        && s.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        })
}

fn one_arg(
    args: &[(Range<usize>, Vec<Piece>)],
    end: usize,
    spans: &mut Spans,
) -> Result<Vec<u8>, ParseError> {
    let Some((r, p)) = args.first() else {
        return fail(end, ParseErrorKind::MissingArg);
    };
    if p.is_empty() {
        return fail(r.start, ParseErrorKind::MissingArg);
    }
    spans.push((r.clone(), TokenClass::Value));
    if let Some((r, _)) = args.get(1) {
        return fail(r.start, ParseErrorKind::BadTerm);
    }
    Ok(bytes(p))
}

fn count(
    args: &[(Range<usize>, Vec<Piece>)],
    spans: &mut Spans,
) -> Result<Option<u32>, ParseError> {
    let Some((r, p)) = args.first() else {
        return Ok(None);
    };
    let n = plain_text(p)
        .filter(|t| !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|t| t.parse::<u32>().ok());
    let Some(n) = n else {
        return fail(r.start, ParseErrorKind::BadNumber);
    };
    spans.push((r.clone(), TokenClass::Value));
    if let Some((r, _)) = args.get(1) {
        return fail(r.start, ParseErrorKind::BadTerm);
    }
    Ok(Some(n))
}

fn dest(
    args: &[(Range<usize>, Vec<Piece>)],
    end: usize,
    spans: &mut Spans,
) -> Result<Dest, ParseError> {
    let Some((r, p)) = args.first() else {
        return fail(end, ParseErrorKind::MissingDest);
    };
    if !plain_prefix(p, b">") {
        return fail(r.start, ParseErrorKind::MissingDest);
    }
    // `> here`: a lone `>` takes the next word.
    let (body, span, rest) = if p.len() == 1 {
        match args.get(1) {
            Some((r2, p2)) => (&p2[..], r.start..r2.end, &args[2..]),
            None => return fail(end, ParseErrorKind::MissingDest),
        }
    } else {
        (&p[1..], r.clone(), &args[1..])
    };
    if body.is_empty() {
        return fail(span.start, ParseErrorKind::MissingDest);
    }
    spans.push((span, TokenClass::Dest));
    if let Some((r, _)) = rest.first() {
        return fail(r.start, ParseErrorKind::BadTerm);
    }
    Ok(if plain_eq(body, b"other") {
        Dest::Other
    } else if plain_eq(body, b"here") {
        Dest::Here
    } else {
        Dest::Path(bytes(body))
    })
}

const UNITS_SIZE: &[(u8, u64)] = &[
    (b'K', 1 << 10),
    (b'M', 1 << 20),
    (b'G', 1 << 30),
    (b'T', 1 << 40),
];
pub(crate) const UNITS_AGE: &[(u8, u64)] = &[
    (b's', 1),
    (b'm', 60),
    (b'h', 3600),
    (b'd', 86_400),
    (b'w', 604_800),
];

fn term(r: &Range<usize>, p: &[Piece], spans: &mut Spans) -> Result<Term, ParseError> {
    let bad = |kind| fail(r.start, kind);
    let negated = plain_prefix(p, b"!");
    let body = if negated { &p[1..] } else { p };
    if body.is_empty() {
        return bad(ParseErrorKind::BadTerm);
    }
    let lead = usize::from(negated);
    // The key span: the leading `!` plus the key's plain bytes.
    let key = |len: usize, spans: &mut Spans| {
        let split = p.get(lead + len).map_or(r.end, |q| q.at);
        if split > r.start {
            spans.push((r.start..split, TokenClass::Key));
        }
        if split < r.end {
            spans.push((split..r.end, TokenClass::Value));
        }
    };
    let pred = if plain_prefix(body, b"ext:") {
        key(4, spans);
        let mut list = vec![Vec::new()];
        for q in &body[4..] {
            if q.plain && q.byte == b',' {
                list.push(Vec::new());
            } else if let Some(last) = list.last_mut() {
                last.push(q.byte);
            }
        }
        Pred::Ext(list)
    } else if plain_prefix(body, b"name:") {
        key(5, spans);
        Pred::Name(bytes(&body[5..]))
    } else if plain_prefix(body, b"type:") {
        key(5, spans);
        let rest = &body[5..];
        let k = if plain_eq(rest, b"file") {
            KindSel::File
        } else if plain_eq(rest, b"dir") {
            KindSel::Dir
        } else if plain_eq(rest, b"link") {
            KindSel::Link
        } else {
            return bad(ParseErrorKind::BadTerm);
        };
        Pred::Kind(k)
    } else if let Some((cmp, len)) = keyed_cmp(body, b"size") {
        key(len, spans);
        Pred::Size(
            cmp,
            number(&body[len..], UNITS_SIZE, true).ok_or(ParseError {
                offset: r.start,
                kind: ParseErrorKind::BadNumber,
            })?,
        )
    } else if let Some((cmp, len)) = keyed_cmp(body, b"age") {
        key(len, spans);
        Pred::Age(
            cmp,
            number(&body[len..], UNITS_AGE, false).ok_or(ParseError {
                offset: r.start,
                kind: ParseErrorKind::BadNumber,
            })?,
        )
    } else {
        key(0, spans);
        Pred::Name(bytes(body))
    };
    Ok(Term { negated, pred })
}

/// `size>=` → (`Ge`, 6): the comparison after a plain key, and its length.
fn keyed_cmp(p: &[Piece], key: &[u8]) -> Option<(Cmp, usize)> {
    if !plain_prefix(p, key) {
        return None;
    }
    let rest = &p[key.len()..];
    for (op, cmp) in [
        (&b"<="[..], Cmp::Le),
        (b">=", Cmp::Ge),
        (b"<", Cmp::Lt),
        (b">", Cmp::Gt),
        (b"=", Cmp::Eq),
    ] {
        if plain_prefix(rest, op) {
            return Some((cmp, key.len() + op.len()));
        }
    }
    None
}

/// `1.5M` → 1572864; `30d` → 2592000. Sizes may omit the unit (bytes) and
/// take a fraction; ages need a unit and are whole.
fn number(p: &[Piece], units: &[(u8, u64)], size: bool) -> Option<u64> {
    let text = plain_text(p)?;
    let t = text.as_bytes();
    let digits_end = t
        .iter()
        .position(|b| !b.is_ascii_digit())
        .unwrap_or(t.len());
    if digits_end == 0 {
        return None;
    }
    let int: u128 = text[..digits_end].parse().ok()?;
    let mut rest = &t[digits_end..];
    let mut frac: (u128, u32) = (0, 0);
    if size && rest.first() == Some(&b'.') {
        let f_end = rest[1..]
            .iter()
            .position(|b| !b.is_ascii_digit())
            .map_or(rest.len(), |i| i + 1);
        let f = std::str::from_utf8(&rest[1..f_end]).ok()?;
        if f.is_empty() || f.len() > 9 {
            return None;
        }
        frac = (f.parse().ok()?, u32::try_from(f.len()).ok()?);
        rest = &rest[f_end..];
    }
    let mult: u128 = match rest {
        [] if size => 1,
        [u] => {
            let u = if size { u.to_ascii_uppercase() } else { *u };
            u128::from(units.iter().find(|(c, _)| *c == u)?.1)
        }
        _ => return None,
    };
    let value = int.checked_mul(mult)? + frac.0 * mult / 10u128.pow(frac.1);
    u64::try_from(value).ok()
}
