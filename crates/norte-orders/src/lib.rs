//! The order grammar typed in the box: `sel ext:pdf age<30d | cp >other`.
//!
//! Parser, printer and the reversible byte escape, and nothing else: no I/O
//! and no matching. Evaluating an order against a pane is the frontend's job,
//! because only it knows how names fold (`norte-frontend::orders`).
//!
//! Words split on any Unicode space outside quotes and escapes, not only
//! ASCII ones. In a `sel`, `name:GLOB` (or a bare glob) matches by pattern
//! and `name=NAME` names one file exactly, `*` and `?` included as letters.
//!
//! ```
//! use norte_orders::{parse, Pred, Stage};
//! let o = parse("sel name=*").expect("parse");
//! let Stage::Sel(q) = &o.stages[0] else { unreachable!() };
//! assert_eq!(q.terms[0].pred, Pred::Literal(b"*".to_vec()));
//! ```
#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod ast;
mod lex;
mod parse;
mod print;

pub use ast::{Cmp, Dest, KindSel, Order, Pred, Query, Stage, Term};
pub use parse::{ParseError, ParseErrorKind, TokenClass, parse, tokens};
pub use print::{escape_name, print};

/// The verbs that take arguments, for autocompletion. Anything else with a
/// `.` in it is a catalogue command.
///
/// ```
/// assert!(norte_orders::VERBS.contains(&"sel"));
/// ```
pub const VERBS: &[&str] = &["sel", "cp", "mv", "trash", "mkdir", "cd", "log", "undo"];

/// The keys a `sel` term can start with, for autocompletion.
///
/// ```
/// assert!(norte_orders::QUERY_KEYS.contains(&"ext:"));
/// ```
pub const QUERY_KEYS: &[&str] = &[
    "ext:", "name:", "name=", "size>", "size<", "age<", "age>", "type:",
];
