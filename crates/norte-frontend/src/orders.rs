//! An order (`norte-orders`) evaluated against the pane: which rows a `sel`
//! matches, and the steps a frontend runs with the launchers it already has.
//!
//! Names are matched with the SAME fold and glob engine as the mark-by-pattern
//! modal ([`crate::PaneState::mark_glob`]), so `sel` and that modal never
//! disagree on which file is which.

use norte_encoding::NameEncoding;
use norte_orders::{Cmp, Dest, KindSel, Order, Pred, Query, Stage};
use norte_proto::{Entry, EntryKind, Segment, VPath};

use crate::pane::{NameMatcher, PatternError};

/// How many journal rows `log` lists when no count is given.
pub const LOG_DEFAULT: u32 = 20;

/// A query with its globs compiled, ready to test rows.
pub struct CompiledQuery {
    terms: Vec<(bool, Compiled)>,
    /// A `type:dir` term asks for directories, so `size` may look at them.
    dirs_asked: bool,
    enc: Option<NameEncoding>,
}

enum Compiled {
    Ext(Vec<String>),
    Name(NameMatcher),
    Size(Cmp, u64),
    Age(Cmp, u64),
    Kind(KindSel),
}

impl CompiledQuery {
    /// Compiles `q` for names decoded with `enc` (the pane's encoding).
    ///
    /// # Errors
    /// [`PatternError::Glob`] if a glob does not compile.
    pub fn new(q: &Query, enc: Option<NameEncoding>) -> Result<Self, PatternError> {
        let mut terms = Vec::with_capacity(q.terms.len());
        for t in &q.terms {
            let c = match &t.pred {
                Pred::Ext(list) => {
                    Compiled::Ext(list.iter().map(|e| crate::nav::fold(e)).collect())
                }
                Pred::Name(g) => Compiled::Name(NameMatcher::new(g)?),
                Pred::Size(c, n) => Compiled::Size(*c, *n),
                Pred::Age(c, n) => Compiled::Age(*c, *n),
                Pred::Kind(k) => Compiled::Kind(*k),
            };
            terms.push((t.negated, c));
        }
        let dirs_asked = q
            .terms
            .iter()
            .any(|t| !t.negated && t.pred == Pred::Kind(KindSel::Dir));
        Ok(Self {
            terms,
            dirs_asked,
            enc,
        })
    }
}

/// Whether `entry` satisfies every term of `q`. `now_ms` is the clock `age`
/// measures against.
///
/// A term whose answer is UNKNOWN — no mtime, no size, or a directory's size
/// when `type:dir` was not asked for (that size is the inode's, not the
/// tree's) — is false whether negated or not: `!size>1G` must not sweep up
/// every row nobody measured.
#[must_use]
pub fn matches(entry: &Entry, q: &CompiledQuery, now_ms: i64) -> bool {
    let name = entry.path.file_name().map_or(&b""[..], |n| n.as_bytes());
    q.terms.iter().all(|(negated, c)| {
        let known = match c {
            Compiled::Ext(list) => {
                let ext = name
                    .iter()
                    .rposition(|b| *b == b'.')
                    .filter(|i| *i > 0)
                    .map(|i| crate::nav::fold_with(&name[i + 1..], q.enc));
                Some(ext.is_some_and(|e| list.contains(&e)))
            }
            Compiled::Name(m) => Some(m.is_match(name, q.enc)),
            Compiled::Size(cmp, n) => {
                let measured = entry.kind != EntryKind::Dir || q.dirs_asked;
                entry.size.filter(|_| measured).map(|s| cmp.holds(s, *n))
            }
            Compiled::Age(cmp, secs) => entry.mtime_ms.map(|m| {
                let age = u64::try_from(now_ms.saturating_sub(m) / 1000).unwrap_or(0);
                cmp.holds(age, *secs)
            }),
            Compiled::Kind(k) => Some(matches!(
                (k, entry.kind),
                (KindSel::File, EntryKind::File)
                    | (KindSel::Dir, EntryKind::Dir)
                    | (KindSel::Link, EntryKind::Symlink)
            )),
        };
        known.is_some_and(|hit| hit != *negated)
    })
}

/// What a frontend must do, in order. Paths are already resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Replace the focused pane's marks with what the query matches.
    Select(Query),
    /// Copy the marks (or the cursor row) into this folder.
    Copy {
        /// Destination folder.
        to: VPath,
    },
    /// Move them into this folder.
    Move {
        /// Destination folder.
        to: VPath,
    },
    /// Send them to the trash.
    Trash,
    /// Create this folder.
    Mkdir {
        /// The folder to create.
        path: VPath,
    },
    /// Navigate the focused pane here.
    Cd {
        /// Where to go.
        to: VPath,
    },
    /// List the last N journal operations.
    Log(u32),
    /// Undo the last N journal operations.
    Undo(u32),
    /// Run this catalogue command as its key would.
    Command(String),
}

/// The folders an order resolves against.
#[derive(Debug, Clone, Copy)]
pub struct Context<'a> {
    /// The focused pane's folder.
    pub here: &'a VPath,
    /// The other pane's folder, if there is one.
    pub other: Option<&'a VPath>,
}

/// Why an order that parsed cannot run here.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OrderError {
    /// `>other` with no second pane.
    #[error("no other pane")]
    NoOtherPane,
    /// A path that does not make a `VPath`.
    #[error("bad path")]
    BadPath,
    /// Not a catalogue command; the nearest one, if any is close.
    #[error("unknown command")]
    UnknownCommand {
        /// The closest catalogue id.
        nearest: Option<String>,
    },
}

/// Resolves an order into steps.
///
/// # Errors
/// [`OrderError`] when a destination or a command does not exist here.
pub fn steps(order: &Order, cx: &Context<'_>) -> Result<Vec<Step>, OrderError> {
    let dest = |d: &Dest| match d {
        Dest::Other => cx.other.cloned().ok_or(OrderError::NoOtherPane),
        Dest::Here => Ok(cx.here.clone()),
        Dest::Path(p) => resolve(cx.here, p),
    };
    order
        .stages
        .iter()
        .map(|s| {
            Ok(match s {
                Stage::Sel(q) => Step::Select(q.clone()),
                Stage::Cp(d) => Step::Copy { to: dest(d)? },
                Stage::Mv(d) => Step::Move { to: dest(d)? },
                Stage::Trash => Step::Trash,
                Stage::Mkdir(n) => Step::Mkdir {
                    path: resolve(cx.here, n)?,
                },
                Stage::Cd(p) => Step::Cd {
                    to: resolve(cx.here, p)?,
                },
                Stage::Log(n) => Step::Log(n.unwrap_or(LOG_DEFAULT)),
                Stage::Undo(n) => Step::Undo(n.unwrap_or(1)),
                Stage::Catalogue(id) => Step::Command(command(id)?),
            })
        })
        .collect()
}

/// A catalogue id as it binds today: renamed names follow their rename.
fn command(id: &str) -> Result<String, OrderError> {
    use crate::keymap::catalogue::{CATALOGUE, lookup, renamed_to};
    if lookup(id).is_some() {
        return Ok(id.to_owned());
    }
    if let Some(new) = renamed_to(id) {
        return Ok(new.to_owned());
    }
    let nearest = CATALOGUE
        .iter()
        .filter_map(|d| crate::fuzzy::score(id, d.name).map(|m| (m.score, d.name)))
        .max_by_key(|(s, _)| *s)
        .map(|(_, n)| n.to_owned());
    Err(OrderError::UnknownCommand { nearest })
}

/// A typed path against the focused folder: a full `scheme://` form, an
/// absolute `/…` on the same provider, or relative. `.` and `..` are walked
/// here because a [`Segment`] may be neither.
fn resolve(here: &VPath, typed: &[u8]) -> Result<VPath, OrderError> {
    if let Ok(s) = std::str::from_utf8(typed)
        && s.contains("://")
    {
        return VPath::parse(s).map_err(|_| OrderError::BadPath);
    }
    let mut at = if typed.first() == Some(&b'/') {
        let mut root = here.clone();
        while let Some(up) = root.parent() {
            root = up;
        }
        root
    } else {
        here.clone()
    };
    for part in typed.split(|b| *b == b'/') {
        match part {
            b"" | b"." => {}
            b".." => at = at.parent().unwrap_or(at),
            name => {
                let seg = Segment::new(name.to_vec()).map_err(|_| OrderError::BadPath)?;
                at = at.join(seg);
            }
        }
    }
    Ok(at)
}

/// The steps that may run once `Select` reported `matched` rows: none of the
/// mutations when it matched zero, because an empty mark set falls back to
/// the row under the cursor ([`crate::PaneState::marked_paths`]) and `sel`
/// matching nothing must never become "act on whatever the cursor is on".
///
/// ```
/// use norte_frontend::orders::{runnable, Step};
/// let s = [Step::Select(Default::default()), Step::Trash];
/// assert!(runnable(&s, Some(0)).is_empty());
/// assert_eq!(runnable(&s, Some(3)).len(), 2);
/// ```
#[must_use]
pub fn runnable(steps: &[Step], matched: Option<usize>) -> &[Step] {
    if matched == Some(0) { &[] } else { steps }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PaneState;

    const NOW: i64 = 1_800_000_000_000;
    const DAY: i64 = 86_400_000;

    fn dir() -> VPath {
        VPath::parse("mem:///d").expect("vpath")
    }

    fn entry(name: &[u8], kind: EntryKind, size: Option<u64>, age_days: i64) -> Entry {
        Entry {
            path: dir().join(Segment::new(name.to_vec()).expect("segment")),
            kind,
            size,
            mtime_ms: Some(NOW - age_days * DAY),
            attrs: std::collections::BTreeMap::default(),
        }
    }

    fn file(name: &str, size: u64, age_days: i64) -> Entry {
        entry(name.as_bytes(), EntryKind::File, Some(size), age_days)
    }

    fn query(line: &str) -> Query {
        match norte_orders::parse(line).expect("parse").stages.remove(0) {
            Stage::Sel(q) => q,
            s => panic!("{s:?}"),
        }
    }

    fn names(p: &PaneState) -> Vec<String> {
        p.marked_entries()
            .iter()
            .map(|e| {
                String::from_utf8_lossy(e.path.file_name().expect("name").as_bytes()).into_owned()
            })
            .collect()
    }

    fn pane(entries: Vec<Entry>) -> PaneState {
        PaneState::new(dir(), entries)
    }

    #[test]
    fn sel_marks_by_extension_size_and_age() {
        let mut p = pane(vec![
            file("a.pdf", 2 << 20, 1),
            file("b.pdf", 10, 400),
            file("c.txt", 2 << 20, 1),
        ]);
        assert_eq!(p.select_query(&query("sel ext:pdf size>1M"), NOW), Ok(1));
        assert_eq!(names(&p), ["a.pdf"]);
        assert_eq!(p.select_query(&query("sel ext:pdf age>30d"), NOW), Ok(1));
        assert_eq!(names(&p), ["b.pdf"], "select REPLACES the marks");
    }

    #[test]
    fn ext_compares_folded_like_mark_glob() {
        let mut p = pane(vec![
            file("X.PDF", 1, 1),
            entry("e\u{301}.pdf".as_bytes(), EntryKind::File, Some(1), 1),
            file("x.txt", 1, 1),
        ]);
        assert_eq!(p.select_query(&query("sel ext:Pdf"), NOW), Ok(2));
    }

    #[test]
    fn negation_and_bare_globs() {
        let mut p = pane(vec![
            file("a.rs", 1, 1),
            file("a_test.rs", 1, 1),
            file("b.md", 1, 1),
        ]);
        assert_eq!(p.select_query(&query("sel *.rs !*_test*"), NOW), Ok(1));
        assert_eq!(names(&p), ["a.rs"]);
    }

    #[test]
    fn sel_and_the_glob_modal_agree_on_every_hostile_name() {
        let entries: Vec<Entry> = norte_testkit::corpus::hostile_names()
            .into_iter()
            .filter_map(|n| Segment::new(n.bytes).ok())
            .map(|s| Entry {
                path: dir().join(s),
                kind: EntryKind::File,
                size: Some(1),
                mtime_ms: Some(NOW),
                attrs: std::collections::BTreeMap::default(),
            })
            .collect();
        let mut a = pane(entries.clone());
        let mut b = pane(entries);
        a.mark_glob("*a*", true).expect("glob");
        b.select_query(&query("sel *a*"), NOW).expect("sel");
        assert_eq!(a.marked_paths(), b.marked_paths());
    }

    #[test]
    fn zero_matches_never_falls_back_to_the_cursor() {
        let mut p = pane(vec![file("a.txt", 1, 1)]);
        let matched = p.select_query(&query("sel ext:pdf"), NOW).expect("sel");
        assert_eq!(matched, 0);
        let order = norte_orders::parse("sel ext:pdf | cp >here").expect("parse");
        let d = dir();
        let all = steps(
            &order,
            &Context {
                here: &d,
                other: None,
            },
        )
        .expect("steps");
        assert_eq!(runnable(&all, Some(matched)), &[] as &[Step]);
        assert_eq!(runnable(&all, Some(1)).len(), 2);
    }

    #[test]
    fn a_directory_never_matches_size_without_type_dir() {
        let mut p = pane(vec![entry(b"big", EntryKind::Dir, Some(1 << 30), 1)]);
        assert_eq!(p.select_query(&query("sel size>1K"), NOW), Ok(0));
        assert_eq!(p.select_query(&query("sel !size>1K"), NOW), Ok(0));
        assert_eq!(p.select_query(&query("sel type:dir size>1K"), NOW), Ok(1));
    }

    #[test]
    fn unknown_mtime_never_matches_age() {
        let mut e = file("a", 1, 1);
        e.mtime_ms = None;
        let mut p = pane(vec![e]);
        assert_eq!(p.select_query(&query("sel age<1d"), NOW), Ok(0));
        assert_eq!(p.select_query(&query("sel !age<1d"), NOW), Ok(0));
    }

    #[test]
    fn steps_resolve_other_here_and_relative_paths() {
        let here = dir();
        let other = VPath::parse("mem:///o").expect("vpath");
        let cx = Context {
            here: &here,
            other: Some(&other),
        };
        let s = |line: &str| steps(&norte_orders::parse(line).expect("parse"), &cx).expect("steps");
        assert_eq!(s("cp >other"), [Step::Copy { to: other.clone() }]);
        assert_eq!(s("mv >here"), [Step::Move { to: here.clone() }]);
        assert_eq!(
            s("cp >sub/deeper"),
            [Step::Copy {
                to: VPath::parse("mem:///d/sub/deeper").expect("vpath")
            }]
        );
        assert_eq!(
            s("cd /etc"),
            [Step::Cd {
                to: VPath::parse("mem:///etc").expect("vpath")
            }]
        );
        assert_eq!(
            s("mkdir new"),
            [Step::Mkdir {
                path: VPath::parse("mem:///d/new").expect("vpath")
            }]
        );
        assert_eq!(s("log"), [Step::Log(LOG_DEFAULT)]);
        assert_eq!(s("undo"), [Step::Undo(1)]);
    }

    #[test]
    fn a_non_utf8_destination_keeps_its_bytes() {
        let here = dir();
        let cx = Context {
            here: &here,
            other: None,
        };
        let order = norte_orders::parse(r"cp >x\xff").expect("parse");
        let [Step::Copy { to }] = &steps(&order, &cx).expect("steps")[..] else {
            panic!()
        };
        assert_eq!(to.file_name().expect("name").as_bytes(), b"x\xff");
    }

    #[test]
    fn other_without_a_second_pane_is_an_error() {
        let here = dir();
        let cx = Context {
            here: &here,
            other: None,
        };
        let order = norte_orders::parse("cp >other").expect("parse");
        assert_eq!(steps(&order, &cx), Err(OrderError::NoOtherPane));
    }

    #[test]
    fn an_unknown_catalogue_id_suggests_the_nearest() {
        let here = dir();
        let cx = Context {
            here: &here,
            other: None,
        };
        let order = norte_orders::parse("pane.swp").expect("parse");
        assert_eq!(
            steps(&order, &cx),
            Err(OrderError::UnknownCommand {
                nearest: Some("pane.swap".into())
            })
        );
        let ok = norte_orders::parse("pane.swap").expect("parse");
        assert_eq!(steps(&ok, &cx), Ok(vec![Step::Command("pane.swap".into())]));
    }

    #[test]
    fn select_query_is_undone_by_restore_previous_marks() {
        let mut p = pane(vec![file("a.pdf", 1, 1), file("b.txt", 1, 1)]);
        p.mark_all();
        p.select_query(&query("sel ext:pdf"), NOW).expect("sel");
        assert_eq!(p.restore_previous_marks(), Some(2));
    }
}
