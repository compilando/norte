//! From an [`Order`] back to a line that parses to it.

use std::fmt::Write as _;

use crate::ast::{Dest, KindSel, Order, Pred, Stage, Term};
use crate::parse::UNITS_AGE;

/// Prints an order so that [`crate::parse`] gives it back, byte for byte.
///
/// Printable UTF-8 letters and ASCII punctuation are written as they are.
/// Everything else in a name — a byte that is not UTF-8, a control, a space,
/// an invisible or bidi character, `|`, `'`, `\`, and `,` inside `ext:` — is
/// written `\xHH`, one per byte, so a printed order never carries a byte a
/// terminal would act on. A name that would read as syntax (`other` after
/// `>`, a glob starting with `!` or a key) is written so it cannot.
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
        Stage::Catalogue(id) => id.clone(),
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
const INVISIBLE_LETTERS: &[char] = &['\u{115F}', '\u{1160}', '\u{3164}', '\u{FFA0}'];

fn safe(c: char, comma: bool) -> bool {
    if c.is_ascii() {
        c.is_ascii_graphic() && !matches!(c, '|' | '\'' | '\\') && !(comma && c == ',')
    } else {
        c.is_alphanumeric() && !INVISIBLE_LETTERS.contains(&c)
    }
}

fn escape(bytes: &[u8], comma: bool) -> String {
    let mut out = String::with_capacity(bytes.len());
    for chunk in bytes.utf8_chunks() {
        for c in chunk.valid().chars() {
            if safe(c, comma) {
                out.push(c);
            } else {
                let mut buf = [0u8; 4];
                for b in c.encode_utf8(&mut buf).bytes() {
                    let _ = write!(out, "\\x{b:02x}");
                }
            }
        }
        for b in chunk.invalid() {
            let _ = write!(out, "\\x{b:02x}");
        }
    }
    out
}
