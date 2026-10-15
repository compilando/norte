//! From an [`Order`] back to a line that parses to it.

use std::fmt::Write as _;

use crate::ast::{Dest, KindSel, Order, Pred, Stage, Term};
use crate::parse::UNITS_AGE;

/// Prints an order so that [`crate::parse`] gives it back, byte for byte.
///
/// Printable UTF-8 letters and ASCII punctuation are written as they are,
/// and so is a combining mark right after a letter (`é` decomposed, Hindi).
/// Everything else in a name — a byte that is not UTF-8, a control, any
/// space, an invisible or bidi character, a mark with no letter before it,
/// `|`, `'`, `\`, and `,` inside `ext:` — is written `\xHH`, one per byte, so
/// a printed order never carries a byte a terminal would act on. A name that
/// would read as syntax (`other` after `>`, a glob starting with `!` or a
/// key) is written so it cannot.
///
/// Total: any tree prints, none panics. A tree the parser can produce reads
/// back equal; one it cannot (no stages, an empty `sel`, `ext:` with no
/// list, an empty name, a catalogue id that is not one) prints the nearest
/// line, which does not parse back to it.
///
/// ```
/// use norte_orders::{parse, print};
/// let o = parse(r"sel ext:pdf | cp >/mnt/a\ b").expect("parse");
/// assert_eq!(print(&o), r"sel ext:pdf | cp >/mnt/a\x20b");
/// assert_eq!(parse(&print(&o)), Ok(o));
/// ```
#[must_use]
pub fn print(order: &Order) -> String {
    order
        .stages
        .iter()
        .map(stage)
        .collect::<Vec<_>>()
        .join(" | ")
}

/// One name written the way the grammar reads it back as exactly its bytes
/// — the printer's own escape. What is shown to someone who must write the
/// name in an order (the assistant, ADR 0186).
///
/// ```
/// use norte_orders::{escape_name, parse, Stage};
/// let e = escape_name(b"two words\xff");
/// assert_eq!(e, r"two\x20words\xff");
/// let o = parse(&format!("mkdir {e}")).expect("parse");
/// assert_eq!(o.stages, vec![Stage::Mkdir(b"two words\xff".to_vec())]);
/// ```
#[must_use]
pub fn escape_name(bytes: &[u8]) -> String {
    word(bytes)
}

fn stage(s: &Stage) -> String {
    match s {
        Stage::Sel(q) => {
            let mut out = String::from("sel");
            for t in &q.terms {
                out.push(' ');
                out.push_str(&term(t));
            }
            out
        }
        Stage::Cp(d) => format!("cp >{}", dest(d)),
        Stage::Mv(d) => format!("mv >{}", dest(d)),
        Stage::Trash => "trash".to_owned(),
        Stage::Mkdir(n) => format!("mkdir {}", word(n)),
        Stage::Cd(n) => format!("cd {}", word(n)),
        Stage::Log(n) => count("log", *n),
        Stage::Undo(n) => count("undo", *n),
        Stage::Catalogue(id) if crate::parse::is_catalogue_id(id) => id.clone(),
        // A hand-built "id" the parser could never have read: escaped like a
        // name, so it reads back as an unknown verb and never paints raw.
        Stage::Catalogue(id) => word(id.as_bytes()),
    }
}

fn count(verb: &str, n: Option<u32>) -> String {
    n.map_or_else(|| verb.to_owned(), |n| format!("{verb} {n}"))
}

fn dest(d: &Dest) -> String {
    match d {
        Dest::Other => "other".to_owned(),
        Dest::Here => "here".to_owned(),
        // A folder literally called `other`/`here`: its first byte escaped,
        // so it is not read as the keyword.
        Dest::Path(p) if p == b"other" || p == b"here" => {
            format!("\\x{:02x}{}", p[0], escape(&p[1..], false))
        }
        Dest::Path(p) => escape(p, false),
    }
}

fn word(n: &[u8]) -> String {
    if n.is_empty() {
        "''".to_owned()
    } else {
        escape(n, false)
    }
}

fn term(t: &Term) -> String {
    let mut out = String::new();
    if t.negated {
        out.push('!');
    }
    match &t.pred {
        Pred::Ext(list) => {
            out.push_str("ext:");
            let parts: Vec<String> = list.iter().map(|e| escape(e, true)).collect();
            out.push_str(&parts.join(","));
        }
        Pred::Name(n) => {
            let e = escape(n, false);
            if e.is_empty() || reads_as_syntax(&e) {
                out.push_str("name:");
            }
            out.push_str(&e);
        }
        Pred::Literal(n) => {
            out.push_str("name=");
            out.push_str(&escape(n, false));
        }
        Pred::Size(c, n) => {
            let _ = write!(out, "size{}{}", c.as_str(), size(*n));
        }
        Pred::Age(c, n) => {
            let _ = write!(out, "age{}{}", c.as_str(), age(*n));
        }
        Pred::Kind(k) => out.push_str(match k {
            KindSel::File => "type:file",
            KindSel::Dir => "type:dir",
            KindSel::Link => "type:link",
        }),
    }
    out
}

/// Whether a bare (already escaped) glob would parse as something else.
fn reads_as_syntax(e: &str) -> bool {
    let keyed = |k: &str| {
        e.strip_prefix(k)
            .is_some_and(|r| r.starts_with(['<', '>', '=']))
    };
    e.starts_with('!')
        || e.starts_with("ext:")
        || e.starts_with("name:")
        || e.starts_with("name=")
        || e.starts_with("type:")
        || keyed("size")
        || keyed("age")
}

fn size(n: u64) -> String {
    for (unit, mult) in [
        ('T', 1u64 << 40),
        ('G', 1 << 30),
        ('M', 1 << 20),
        ('K', 1 << 10),
    ] {
        if n != 0 && n.is_multiple_of(mult) {
            return format!("{}{unit}", n / mult);
        }
    }
    n.to_string()
}

fn age(n: u64) -> String {
    for &(unit, mult) in UNITS_AGE.iter().rev() {
        if n != 0 && n.is_multiple_of(mult) {
            return format!("{}{}", n / mult, char::from(unit));
        }
    }
    format!("{n}s")
}

/// Hangul fillers: letters to Unicode, invisible to the reader.
pub(crate) const INVISIBLE_LETTERS: &[char] = &['\u{115F}', '\u{1160}', '\u{3164}', '\u{FFA0}'];

/// Combining marks that are `Default_Ignorable` — invisible, so a twin maker
/// (`norte_encoding::is_terminal_hazard`'s table; this crate may not link it,
/// and `every_terminal_hazard_is_escaped_even_after_a_letter` checks the two
/// agree). The variation selectors are here too: after a letter they select
/// nothing a reader can see.
pub(crate) const INVISIBLE_MARKS: &[(char, char)] = &[
    ('\u{034F}', '\u{034F}'),
    ('\u{17B4}', '\u{17B5}'),
    ('\u{180B}', '\u{180F}'),
    ('\u{FE00}', '\u{FE0F}'),
    ('\u{E0100}', '\u{E01EF}'),
];

/// How many combining marks may stack raw on one letter. Vietnamese needs
/// two and Indic clusters rarely pass three; a taller stack (Zalgo) draws
/// over the lines around it.
const MAX_MARKS: u8 = 4;

/// Whether `c` is written as it is. A combining mark only while `marks_left`
/// (right after a letter, up to [`MAX_MARKS`]): alone it would draw on
/// whatever the terminal printed before it.
fn safe(c: char, comma: bool, marks_left: u8) -> bool {
    if c.is_ascii() {
        c.is_ascii_graphic() && !matches!(c, '|' | '\'' | '\\') && !(comma && c == ',')
    } else if unicode_normalization::char::is_combining_mark(c) {
        marks_left > 0
            && !INVISIBLE_MARKS
                .iter()
                .any(|(lo, hi)| (*lo..=*hi).contains(&c))
    } else {
        c.is_alphanumeric() && !INVISIBLE_LETTERS.contains(&c)
    }
}

fn escape(bytes: &[u8], comma: bool) -> String {
    let mut out = String::with_capacity(bytes.len());
    let mut marks_left = 0u8;
    for chunk in bytes.utf8_chunks() {
        for c in chunk.valid().chars() {
            if safe(c, comma, marks_left) {
                out.push(c);
                marks_left = if unicode_normalization::char::is_combining_mark(c) {
                    marks_left.saturating_sub(1)
                } else if c.is_alphanumeric() {
                    MAX_MARKS
                } else {
                    0
                };
            } else {
                let mut buf = [0u8; 4];
                for b in c.encode_utf8(&mut buf).bytes() {
                    let _ = write!(out, "\\x{b:02x}");
                }
                marks_left = 0;
            }
        }
        for b in chunk.invalid() {
            let _ = write!(out, "\\x{b:02x}");
            marks_left = 0;
        }
    }
    out
}
