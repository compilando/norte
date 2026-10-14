//! What an order means once parsed. Names, globs and paths are BYTES.

/// A whole line: stages joined by `|`.
///
/// ```
/// let o = norte_orders::parse("sel *.rs | trash").expect("parse");
/// assert_eq!(o.stages.len(), 2);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Order {
    /// The stages, in the order they run.
    pub stages: Vec<Stage>,
}

/// One verb and its arguments.
///
/// ```
/// use norte_orders::{parse, Stage};
/// assert_eq!(parse("trash").expect("parse").stages, vec![Stage::Trash]);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stage {
    /// Replaces the pane's marks with the rows the query matches.
    Sel(Query),
    /// Copies the marks (or the row under the cursor) to a destination.
    Cp(Dest),
    /// Moves them.
    Mv(Dest),
    /// Sends them to the trash.
    Trash,
    /// Creates a folder with this name, relative to the focused pane.
    Mkdir(Vec<u8>),
    /// Goes to this path, as typed; the frontend resolves it.
    Cd(Vec<u8>),
    /// Lists the last N journal operations (`None`: the frontend's default).
    Log(Option<u32>),
    /// Undoes the last N journal operations (`None`: one).
    Undo(Option<u32>),
    /// A catalogue command by its stable id, run as its key would run it.
    Catalogue(String),
}

/// Where `cp`/`mv` send things.
///
/// ```
/// use norte_orders::{parse, Dest, Stage};
/// assert_eq!(parse("cp >other").expect("parse").stages, vec![Stage::Cp(Dest::Other)]);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dest {
    /// The other pane's folder.
    Other,
    /// The focused pane's folder.
    Here,
    /// A path as typed: absolute, or relative to the focused pane.
    Path(Vec<u8>),
}

/// The terms of a `sel`, all of which must hold.
///
/// ```
/// let o = norte_orders::parse("sel ext:pdf size>1M").expect("parse");
/// let norte_orders::Stage::Sel(q) = &o.stages[0] else { unreachable!() };
/// assert_eq!(q.terms.len(), 2);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Query {
    /// Implicit AND.
    pub terms: Vec<Term>,
}

/// One predicate, possibly negated with a leading `!`.
///
/// ```
/// let o = norte_orders::parse("sel !*.tmp").expect("parse");
/// let norte_orders::Stage::Sel(q) = &o.stages[0] else { unreachable!() };
/// assert!(q.terms[0].negated);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Term {
    /// Whether the predicate is inverted.
    pub negated: bool,
    /// What is tested.
    pub pred: Pred,
}

/// What a term tests on a row.
///
/// ```
/// use norte_orders::{parse, Cmp, Pred, Stage};
/// let o = parse("sel size>2K").expect("parse");
/// let Stage::Sel(q) = &o.stages[0] else { unreachable!() };
/// assert_eq!(q.terms[0].pred, Pred::Size(Cmp::Gt, 2048));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pred {
    /// `ext:pdf,PDF` — any of these extensions; the frontend compares folded.
    Ext(Vec<Vec<u8>>),
    /// `name:GLOB`, or a bare GLOB.
    Name(Vec<u8>),
    /// Size in bytes; `K`/`M`/`G`/`T` are 1024-based.
    Size(Cmp, u64),
    /// Age of the mtime in seconds; units `s m h d w` (`m` is minutes).
    Age(Cmp, u64),
    /// `type:file|dir|link`.
    Kind(KindSel),
}

/// A comparison.
///
/// ```
/// use norte_orders::Cmp;
/// assert!(Cmp::Le.holds(3, 3) && !Cmp::Lt.holds(3, 3));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cmp {
    /// `<`
    Lt,
    /// `<=`
    Le,
    /// `>`
    Gt,
    /// `>=`
    Ge,
    /// `=`
    Eq,
}

impl Cmp {
    /// Whether `left OP right` holds.
    #[must_use]
    pub fn holds(self, left: u64, right: u64) -> bool {
        match self {
            Self::Lt => left < right,
            Self::Le => left <= right,
            Self::Gt => left > right,
            Self::Ge => left >= right,
            Self::Eq => left == right,
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Lt => "<",
            Self::Le => "<=",
            Self::Gt => ">",
            Self::Ge => ">=",
            Self::Eq => "=",
        }
    }
}

/// The kinds `type:` can ask for.
///
/// ```
/// use norte_orders::{parse, KindSel, Pred, Stage};
/// let o = parse("sel type:dir").expect("parse");
/// let Stage::Sel(q) = &o.stages[0] else { unreachable!() };
/// assert_eq!(q.terms[0].pred, Pred::Kind(KindSel::Dir));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KindSel {
    /// A regular file.
    File,
    /// A directory.
    Dir,
    /// A symbolic link.
    Link,
}
