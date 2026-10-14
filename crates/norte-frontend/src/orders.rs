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
    /// Folded extensions, each with whether it names raw bytes.
    Ext(Vec<(String, bool)>),
    Name(NameMatcher),
    /// `name=`: its bytes, and its NFC key ([`nfc_key`]) — no case folding.
    Literal(Vec<u8>, Option<Vec<u8>>),
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
                Pred::Ext(list) => Compiled::Ext(
                    list.iter()
                        .map(|e| {
                            let exact = std::str::from_utf8(e).is_err();
                            let folded = if exact {
                                crate::nav::fold_exact(e, None)
                            } else {
                                crate::nav::fold(e)
                            };
                            (folded, exact)
                        })
                        .collect(),
                ),
                Pred::Name(g) => Compiled::Name(NameMatcher::new(g)?),
                Pred::Literal(n) => Compiled::Literal(n.clone(), nfc_key(n, None)),
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
                // Decoded and folded WHOLE, then split: the extension is the
                // one `*.X` sees, also when only the stem is not UTF-8. An
                // element naming raw bytes compares losslessly, as a glob
                // naming them does.
                let ext_of = |exact: bool| {
                    let folded = if exact {
                        crate::nav::fold_exact(name, q.enc)
                    } else {
                        crate::nav::fold_with(name, q.enc)
                    };
                    folded
                        .rfind('.')
                        .filter(|i| *i > 0)
                        .and_then(|i| folded.get(i + 1..).map(str::to_owned))
                };
                let (lossy, exact) = (ext_of(false), ext_of(true));
                Some(list.iter().any(|(needle, is_exact)| {
                    let ext = if *is_exact { &exact } else { &lossy };
                    ext.as_deref() == Some(needle.as_str())
                }))
            }
            Compiled::Name(m) => Some(m.is_match(name, q.enc)),
            Compiled::Literal(bytes, key) => Some(
                name == bytes.as_slice()
                    || key.is_some() && nfc_key(name, q.enc).as_ref() == key.as_ref(),
            ),
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
    /// What the focused folder is known to hold (empty: nothing known). A
    /// typed name is matched against it in NFC and through
    /// [`Self::enc`], and the path keeps the listed bytes: `cd café` reaches
    /// a folder stored decomposed, or one a cp866 pane paints as `café`.
    pub listing: &'a [Entry],
    /// The focused pane's name encoding, if it reinterprets names.
    pub enc: Option<NameEncoding>,
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
    /// A stage that acts before the last one: each action opens its own
    /// confirmation, so only one can follow the `sel`s.
    #[error("only the last stage may act")]
    OneAction,
}

/// What `undo N` will do: the `journal.undo_after` arguments and the count
/// the confirmation shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UndoCut {
    /// Undo the reader's entries with a `seq` above this one.
    pub seq: i64,
    /// …and none above this ceiling: an entry written after the listing was
    /// read is not undone without having been counted.
    pub techo: Option<i64>,
    /// What the confirmation says.
    pub summary: crate::timeline::Cutoff,
}

/// Why `undo N` asks nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UndoRefusal {
    /// There is nothing of the reader's to undo.
    Nothing,
    /// N reaches past the page that was read; undoing "everything after 0"
    /// would undo history nobody counted.
    TooFar,
}

/// `undo N` over the reader's newest-first journal page (actor `user`):
/// cut at the (N+1)-th OPERATION — a batch is one, as in the timeline.
///
/// # Errors
/// [`UndoRefusal`] when there is nothing to undo or N goes past the page.
pub fn undo_cut(
    newest_first: &[norte_proto::methods::JournalRow],
    next_before_seq: Option<i64>,
    n: u32,
) -> Result<UndoCut, UndoRefusal> {
    let tl = crate::timeline::Timeline::new(newest_first, next_before_seq);
    let n = usize::try_from(n).unwrap_or(usize::MAX);
    if n == 0
        || !tl
            .rows()
            .iter()
            .any(crate::timeline::TimelineRow::will_undo)
    {
        return Err(UndoRefusal::Nothing);
    }
    // N counts what can still be undone: a compensation or an undone row on
    // top must not make a second `undo` a no-op. The cut is the row right
    // after the N-th such operation — and it must exist, because the core
    // stands on a real row (`undo_after(0)` is refused).
    let mut seen = 0usize;
    let mut cut_at = None;
    for (i, row) in tl.rows().iter().enumerate() {
        if seen == n {
            cut_at = Some(i);
            break;
        }
        if row.will_undo() {
            seen += 1;
        }
    }
    let Some(i) = cut_at else {
        return Err(UndoRefusal::TooFar);
    };
    let seq = tl.rows().get(i).map(|r| r.seq).ok_or(UndoRefusal::TooFar)?;
    Ok(UndoCut {
        seq,
        techo: tl.techo(),
        summary: tl.summary_of_newest(i),
    })
}

/// Whether a row is hidden by the panes' rule (a leading `.` byte): a client
/// with no pane (`norte run`) leaves it out the way a pane with hidden files
/// off does.
///
/// ```
/// # use norte_proto::{Entry, EntryKind, Segment, VPath};
/// let dir = VPath::parse("mem:///d").unwrap();
/// let e = |n: &str| Entry { path: dir.join(Segment::new(n.as_bytes().to_vec()).unwrap()),
///     kind: EntryKind::File, size: None, mtime_ms: None, attrs: Default::default() };
/// assert!(norte_frontend::orders::is_hidden(&e(".env")));
/// assert!(!norte_frontend::orders::is_hidden(&e("a.env")));
/// ```
#[must_use]
pub fn is_hidden(entry: &Entry) -> bool {
    crate::pane::is_hidden_entry(entry)
}

/// How many rows an order measures before its `sel` at most; past this the
/// rest stay unknown and the preview says how many.
pub const MEASURE_CAP: usize = 5_000;

/// The markable rows a `sel` cannot judge yet: it tests `size` or `age` and
/// the lazy listing (#52) never measured them — only painted rows get a
/// stat. An executor stats these and calls [`crate::PaneState::hydrate`]
/// before it selects; the live preview only counts them.
#[must_use]
pub fn unmeasured(pane: &crate::PaneState, q: &Query) -> Vec<VPath> {
    let wants_size = q.terms.iter().any(|t| matches!(t.pred, Pred::Size(..)));
    let wants_age = q.terms.iter().any(|t| matches!(t.pred, Pred::Age(..)));
    if !wants_size && !wants_age {
        return Vec::new();
    }
    pane.markable_entries()
        .filter(|e| {
            (wants_size && e.size.is_none() && e.kind != EntryKind::Dir)
                || (wants_age && e.mtime_ms.is_none())
        })
        .map(|e| e.path.clone())
        .take(MEASURE_CAP)
        .collect()
}

/// The focused pane's EXPLICIT marks — empty when only the cursor row would
/// act, because that fallback is not a mark and must not come back as one.
#[must_use]
pub fn marks_before(pane: &crate::PaneState) -> Vec<VPath> {
    if pane.marks_len() == 0 {
        Vec::new()
    } else {
        pane.marked_paths()
    }
}

/// Puts back the marks the pane had when the box opened.
pub fn restore(pane: &mut crate::PaneState, before: &[VPath]) {
    pane.replace_marks(before);
}

/// The live preview of the box's `query`: starts again from `before`, and
/// when the line parses and opens with `sel`, marks what it matches.
/// Returns what to show under the line (`1 item · 2 KiB`), or empty.
#[must_use]
pub fn preview(
    pane: &mut crate::PaneState,
    query: &str,
    before: &[VPath],
    now_ms: i64,
    lang: norte_i18n::Lang,
) -> String {
    restore(pane, before);
    let Some(body) = query.strip_prefix(crate::goto::PREFIX_ORDER) else {
        return String::new();
    };
    let Ok(order) = norte_orders::parse(body) else {
        return String::new();
    };
    let Some(Stage::Sel(q)) = order.stages.first() else {
        return String::new();
    };
    let missing = unmeasured(pane, q).len();
    match pane.preview_query(q, now_ms) {
        Ok(n) => {
            let n_text = n.to_string();
            let size = crate::human_bytes(pane.marked_bytes());
            let mut text = norte_i18n::ta_in(
                lang,
                "msg-order-preview",
                &[("n", &n_text), ("size", &size)],
            );
            if missing > 0 {
                text.push_str(" · ");
                text.push_str(&norte_i18n::ta_in(
                    lang,
                    "msg-order-unmeasured",
                    &[("n", &missing.to_string())],
                ));
            }
            text
        }
        Err(_) => String::new(),
    }
}

/// The Fluent key that explains a parse error to the reader.
#[must_use]
pub fn parse_error_key(kind: norte_orders::ParseErrorKind) -> &'static str {
    use norte_orders::ParseErrorKind as K;
    match kind {
        K::Empty => "order-error-empty",
        K::UnknownVerb => "order-error-unknown-verb",
        K::NotYet => "order-error-not-yet",
        K::StandsAlone => "order-error-stands-alone",
        K::BadTerm => "order-error-bad-term",
        K::BadNumber => "order-error-bad-number",
        K::BadEscape => "order-error-bad-escape",
        K::MissingDest => "order-error-missing-dest",
        K::MissingArg => "order-error-missing-arg",
        K::Unterminated => "order-error-unterminated",
    }
}

/// An [`OrderError`], translated.
#[must_use]
pub fn order_error_text(e: &OrderError, lang: norte_i18n::Lang) -> String {
    match e {
        OrderError::NoOtherPane => norte_i18n::t_in(lang, "order-error-no-other-pane"),
        OrderError::BadPath => norte_i18n::t_in(lang, "order-error-bad-path"),
        OrderError::OneAction => norte_i18n::t_in(lang, "order-error-one-action"),
        OrderError::UnknownCommand { nearest: None } => {
            norte_i18n::t_in(lang, "order-error-unknown-command")
        }
        OrderError::UnknownCommand { nearest: Some(n) } => norte_i18n::ta_in(
            lang,
            "order-error-unknown-command-nearest",
            &[("nearest", n)],
        ),
    }
}

/// Resolves an order into steps.
///
/// # Errors
/// [`OrderError`] when a destination or a command does not exist here.
pub fn steps(order: &Order, cx: &Context<'_>) -> Result<Vec<Step>, OrderError> {
    let dest = |d: &Dest| match d {
        Dest::Other => cx.other.cloned().ok_or(OrderError::NoOtherPane),
        Dest::Here => Ok(cx.here.clone()),
        Dest::Path(p) => resolve(cx, p),
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
                    path: resolve(cx, n)?,
                },
                Stage::Cd(p) => Step::Cd {
                    to: resolve(cx, p)?,
                },
                Stage::Log(n) => Step::Log(n.unwrap_or(LOG_DEFAULT)),
                Stage::Undo(n) => Step::Undo(n.unwrap_or(1)),
                Stage::Catalogue(id) => Step::Command(command(id)?),
            })
        })
        .collect::<Result<Vec<_>, _>>()
        .and_then(|all| {
            let (_, init) = all.split_last().unwrap_or((&Step::Trash, &[]));
            if init.iter().all(|s| matches!(s, Step::Select(_))) {
                Ok(all)
            } else {
                Err(OrderError::OneAction)
            }
        })
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
/// here because a [`Segment`] may be neither. A name typed inside the focused
/// folder takes the bytes of the entry it names ([`listed`]).
fn resolve(cx: &Context<'_>, typed: &[u8]) -> Result<VPath, OrderError> {
    let (mut at, rest) = match url_root(typed)? {
        Some((root, rest)) => (root, rest),
        None => (start_of(cx.here, typed), typed),
    };
    for part in rest.split(|b| *b == b'/') {
        match part {
            b"" | b"." => {}
            b".." => at = at.parent().unwrap_or(at),
            name => {
                let name = if at == *cx.here {
                    listed(cx, name)
                } else {
                    name
                };
                let seg = Segment::new(name.to_vec()).map_err(|_| OrderError::BadPath)?;
                at = at.join(seg);
            }
        }
    }
    Ok(at)
}

/// The listed name `typed` means: itself if listed byte for byte, else the
/// ONE entry equal to it in NFC (decoded with the pane's encoding where it
/// reinterprets), else `typed` as it is. Two twins and no exact hit: neither
/// is guessed.
fn listed<'b>(cx: &Context<'b>, typed: &'b [u8]) -> &'b [u8] {
    let names = || {
        cx.listing
            .iter()
            .filter_map(|e| e.path.file_name().map(Segment::as_bytes))
    };
    if names().any(|n| n == typed) {
        return typed;
    }
    let Some(want) = nfc_key(typed, None) else {
        return typed;
    };
    let mut hits = names().filter(|n| nfc_key(n, cx.enc).as_ref() == Some(&want));
    match (hits.next(), hits.next()) {
        (Some(one), None) => one,
        _ => typed,
    }
}

/// A name in NFC without case folding; a non-UTF-8 name a pane reinterprets
/// is decoded first, as [`crate::nav::fold_with`] decodes it. `None` when
/// NFC would rewrite a canonical singleton (KELVIN SIGN into `K`): those are
/// two names a reader tells apart, so they never pair.
fn nfc_key(name: &[u8], enc: Option<NameEncoding>) -> Option<Vec<u8>> {
    use unicode_normalization::UnicodeNormalization as _;
    let decoded = match (enc, std::str::from_utf8(name)) {
        (Some(e), Err(_)) => Some(norte_encoding::decode_name(name, e)),
        _ => None,
    };
    match decoded {
        Some(text) if norte_encoding::has_canonical_singleton(text.as_bytes()) => None,
        Some(text) => Some(text.nfc().collect::<String>().into_bytes()),
        None if norte_encoding::has_canonical_singleton(name) => None,
        None => Some(norte_encoding::name_key(name, norte_encoding::FoldMode::None).into_owned()),
    }
}

/// `scheme://authority` at the start of a typed path, detected on ASCII
/// bytes so a non-UTF-8 name further on does not turn the URL into a
/// relative path. The root parses through `VPath`; the segments after it
/// stay raw bytes. A password typed inline is refused (hard rule 10).
fn url_root(typed: &[u8]) -> Result<Option<(VPath, &[u8])>, OrderError> {
    let Some(sep) = typed.windows(3).position(|w| w == b"://") else {
        return Ok(None);
    };
    let scheme = &typed[..sep];
    let is_scheme = scheme.first().is_some_and(u8::is_ascii_lowercase)
        && scheme.iter().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'+' | b'.' | b'-')
        });
    if !is_scheme {
        return Ok(None);
    }
    let after = &typed[sep + 3..];
    let auth_len = after.iter().position(|b| *b == b'/').unwrap_or(after.len());
    let authority = &after[..auth_len];
    if let Some(at) = authority.iter().position(|b| *b == b'@')
        && authority[..at].contains(&b':')
    {
        return Err(OrderError::BadPath);
    }
    let head =
        std::str::from_utf8(&typed[..sep + 3 + auth_len]).map_err(|_| OrderError::BadPath)?;
    let root = VPath::parse(&format!("{head}/")).map_err(|_| OrderError::BadPath)?;
    Ok(Some((root, &after[auth_len..])))
}

/// Where a non-URL typed path starts: the provider's root for `/…`, else
/// the focused folder.
fn start_of(here: &VPath, typed: &[u8]) -> VPath {
    if typed.first() == Some(&b'/') {
        let mut root = here.clone();
        while let Some(up) = root.parent() {
            root = up;
        }
        root
    } else {
        here.clone()
    }
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
                listing: &[],
                enc: None,
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
            listing: &[],
            enc: None,
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
            listing: &[],
            enc: None,
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
            listing: &[],
            enc: None,
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
            listing: &[],
            enc: None,
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

    fn jrow(seq: i64, batch: Option<i64>) -> norte_proto::methods::JournalRow {
        norte_proto::methods::JournalRow {
            undoes_seq: None,
            undone: false,
            hostile: false,
            seq,
            ts_ms: seq,
            actor_kind: "user".to_owned(),
            actor_id: None,
            op: "copied".to_owned(),
            path: format!("file:///a/{seq}"),
            path_to: None,
            reversible: true,
            batch_id: batch,
        }
    }

    #[test]
    fn undo_one_cuts_at_the_second_newest_operation() {
        let rows = [
            jrow(9, None),
            jrow(8, Some(1)),
            jrow(7, Some(1)),
            jrow(6, None),
        ];
        let cut = undo_cut(&rows, None, 1).expect("cut");
        assert_eq!(cut.seq, 8, "the batch 8+7 is one operation and it stays");
        assert_eq!(cut.summary.to_undo, 1);
        let two = undo_cut(&rows, None, 2).expect("cut");
        assert_eq!(two.seq, 6);
        assert_eq!(two.summary.to_undo, 3, "the batch counts its members");
    }

    #[test]
    fn the_ceiling_is_the_newest_listed_seq() {
        let rows = [jrow(9, None), jrow(6, None)];
        assert_eq!(undo_cut(&rows, None, 1).expect("cut").techo, Some(9));
    }

    /// The core refuses `undo_after(0)`: there is no row 0 to stand on. Undoing
    /// past the oldest row is the timeline's job, not a broken dialog's.
    #[test]
    fn undo_past_the_oldest_row_is_refused() {
        let rows = [jrow(9, None), jrow(6, None)];
        assert_eq!(undo_cut(&rows, None, 2), Err(UndoRefusal::TooFar));
        assert_eq!(undo_cut(&rows, Some(6), 5), Err(UndoRefusal::TooFar));
    }

    /// `undo` twice: the first leaves a compensation row on top and its
    /// target marked undone. N counts what can still be undone, so the second
    /// `undo` reaches the operation before.
    #[test]
    fn undo_counts_only_what_can_still_be_undone() {
        let mut comp = jrow(7, None);
        comp.undoes_seq = Some(6);
        let mut done = jrow(6, None);
        done.undone = true;
        let rows = [comp, done, jrow(5, None), jrow(4, None)];
        let cut = undo_cut(&rows, None, 1).expect("cut");
        assert_eq!(cut.seq, 4, "undoes 5, the newest still undoable");
        assert_eq!(cut.summary.to_undo, 1);
    }

    /// `\xff.rs` and `\xfe.rs` both read `�.rs` lossily; an escaped byte
    /// targets one of them, never its twin.
    #[test]
    fn an_escaped_byte_never_selects_its_lossy_twin() {
        let mk = |b: &[u8]| entry(b, EntryKind::File, Some(1), 1);
        let mut p = pane(vec![mk(b"\xff.rs"), mk(b"\xfe.rs")]);
        assert_eq!(p.select_query(&query(r"sel name:\xff.rs"), NOW), Ok(1));
        assert_eq!(p.select_query(&query(r"sel ext:rs"), NOW), Ok(2));
        assert_eq!(p.mark_glob("*.rs", true), Ok(0), "both already marked");
    }

    #[test]
    fn a_url_destination_keeps_non_utf8_bytes() {
        let here = dir();
        let cx = Context {
            here: &here,
            other: None,
            listing: &[],
            enc: None,
        };
        let order = norte_orders::parse(r"cp >mem:///o/caf\xe9").expect("parse");
        let [Step::Copy { to }] = &steps(&order, &cx).expect("steps")[..] else {
            panic!()
        };
        assert_eq!(to.scheme(), "mem");
        assert_eq!(to.file_name().expect("name").as_bytes(), b"caf\xe9");
        assert_eq!(to.parent(), Some(VPath::parse("mem:///o").expect("vpath")));
    }

    /// Hard rule 10: a password typed inline never travels on as a path.
    #[test]
    fn a_url_with_an_inline_password_is_refused() {
        let here = dir();
        let cx = Context {
            here: &here,
            other: None,
            listing: &[],
            enc: None,
        };
        let order = norte_orders::parse("cp >sftp://u:pw@h/x").expect("parse");
        assert_eq!(steps(&order, &cx), Err(OrderError::BadPath));
    }

    /// Restoring the box's marks is not a bulk gesture: `mark.restore` must
    /// still give back what the reader had before the box.
    #[test]
    fn the_preview_never_overwrites_the_restore_slot() {
        let mut p = pane(vec![file("a.pdf", 1, 1), file("b.txt", 1, 1)]);
        p.mark_all();
        p.clear_marks();
        let before = marks_before(&p);
        let _ = preview(&mut p, ":sel ext:pdf", &before, NOW, norte_i18n::Lang::En);
        restore(&mut p, &before);
        assert_eq!(
            p.restore_previous_marks(),
            Some(2),
            "the unmark-all is still undoable"
        );
    }

    #[test]
    fn undo_zero_or_nothing_is_refused() {
        assert_eq!(
            undo_cut(&[jrow(9, None)], None, 0),
            Err(UndoRefusal::Nothing)
        );
        assert_eq!(undo_cut(&[], None, 1), Err(UndoRefusal::Nothing));
    }

    #[test]
    fn unmeasured_lists_only_what_the_query_needs_and_is_missing() {
        let mut lazy = file("lazy.bin", 0, 1);
        lazy.size = None;
        lazy.mtime_ms = None;
        let p = pane(vec![lazy, file("known.bin", 5, 1)]);
        let names = |q: &str| -> Vec<String> {
            unmeasured(&p, &query(q))
                .iter()
                .map(|v| {
                    String::from_utf8_lossy(v.file_name().expect("name").as_bytes()).into_owned()
                })
                .collect()
        };
        assert_eq!(names("sel size>1K"), ["lazy.bin"]);
        assert_eq!(names("sel age<1d"), ["lazy.bin"]);
        assert!(names("sel *.bin").is_empty(), "a glob needs no stat");
    }

    #[test]
    fn the_preview_says_how_many_rows_it_could_not_measure() {
        let mut lazy = file("lazy.bin", 0, 1);
        lazy.size = None;
        let mut p = pane(vec![lazy, file("known.bin", 5 << 20, 1)]);
        let text = preview(&mut p, ":sel size>1M", &[], NOW, norte_i18n::Lang::En);
        assert!(text.contains("not measured"), "{text}");
    }

    #[test]
    fn only_the_last_stage_may_act() {
        let here = dir();
        let cx = Context {
            here: &here,
            other: None,
            listing: &[],
            enc: None,
        };
        let order = norte_orders::parse("sel *.rs | cp >here | trash").expect("parse");
        assert_eq!(steps(&order, &cx), Err(OrderError::OneAction));
        let ok = norte_orders::parse("sel *.rs | sel !a* | pane.copy").expect("parse");
        assert_eq!(steps(&ok, &cx).map(|s| s.len()), Ok(3));
    }

    #[test]
    fn the_live_preview_selects_and_says_how_much() {
        let mut p = pane(vec![file("a.pdf", 2048, 1), file("b.txt", 1, 1)]);
        let text = preview(&mut p, ":sel ext:pdf", &[], NOW, norte_i18n::Lang::En);
        assert_eq!(names(&p), ["a.pdf"]);
        assert!(text.contains('1') && text.contains("KiB"), "{text}");
    }

    #[test]
    fn the_live_preview_starts_from_the_marks_before_the_box() {
        let mut p = pane(vec![file("a.pdf", 1, 1), file("b.txt", 1, 1)]);
        p.mark_all();
        let before = marks_before(&p);
        let _ = preview(&mut p, ":sel ext:pdf", &before, NOW, norte_i18n::Lang::En);
        // The reader deletes back to a line with no `sel`: their marks return.
        let text = preview(&mut p, ":tra", &before, NOW, norte_i18n::Lang::En);
        assert_eq!(text, "");
        assert_eq!(p.marks_len(), 2);
    }

    #[test]
    fn marks_before_is_empty_when_only_the_cursor_would_act() {
        let p = pane(vec![file("a", 1, 1)]);
        assert!(
            marks_before(&p).is_empty(),
            "the cursor fallback is not a mark"
        );
    }

    #[test]
    fn restore_puts_back_exactly_the_marks_from_before() {
        let mut p = pane(vec![file("a.pdf", 1, 1), file("b.txt", 1, 1)]);
        let _ = preview(&mut p, ":sel *", &[], NOW, norte_i18n::Lang::En);
        restore(&mut p, &[]);
        assert_eq!(p.marks_len(), 0);
    }

    #[test]
    fn every_parse_error_reads_in_both_locales() {
        use norte_orders::ParseErrorKind as K;
        for lang in [norte_i18n::Lang::En, norte_i18n::Lang::Es] {
            let ids = norte_i18n::message_ids(lang);
            for k in [
                K::Empty,
                K::UnknownVerb,
                K::NotYet,
                K::StandsAlone,
                K::BadTerm,
                K::BadNumber,
                K::BadEscape,
                K::MissingDest,
                K::MissingArg,
                K::Unterminated,
            ] {
                assert!(
                    ids.iter().any(|i| i == parse_error_key(k)),
                    "{lang:?} {k:?}"
                );
            }
            for key in [
                "order-error-no-other-pane",
                "order-error-bad-path",
                "order-error-unknown-command",
                "order-error-unknown-command-nearest",
                "msg-order-preview",
                "msg-order-nothing-matched",
                "goto-prefix-order",
                "goto-hint-order",
            ] {
                assert!(ids.iter().any(|i| i == key), "{lang:?} {key}");
            }
        }
    }

    #[test]
    fn name_equals_targets_a_file_literally_called_star() {
        let mut p = pane(vec![file("*", 1, 1), file("a", 1, 1), file("B?", 1, 1)]);
        assert_eq!(p.select_query(&query("sel name=*"), NOW), Ok(1));
        assert_eq!(names(&p), ["*"]);
        assert_eq!(p.select_query(&query("sel name=B?"), NOW), Ok(1));
        assert_eq!(p.select_query(&query("sel !name=*"), NOW), Ok(2));
        assert_eq!(p.select_query(&query("sel *"), NOW), Ok(3));
    }

    /// `name=` is one name: case does not fold (two files on ext4), a
    /// canonical singleton is not its NFC target, but a decomposed spelling
    /// is the same name.
    #[test]
    fn name_equals_never_reaches_a_case_or_singleton_twin() {
        let mk = |id: &str| entry(&corpus(id), EntryKind::File, Some(1), 1);
        let mut p = pane(vec![
            mk("ascii_case_twin_upper"),
            mk("ascii_case_twin_lower"),
        ]);
        assert_eq!(p.select_query(&query("sel name=Orthodox"), NOW), Ok(1));
        let mut p = pane(vec![mk("singleton_kelvin_sign"), mk("nfd_e_acute")]);
        assert_eq!(p.select_query(&query("sel name=K.txt"), NOW), Ok(0));
        let nfc = String::from_utf8(corpus("nfc_e_acute")).expect("utf8");
        assert_eq!(
            p.select_query(&query(&format!("sel name={nfc}")), NOW),
            Ok(1)
        );
    }

    /// In a pane reading names as an encoding, `name=` matches the exact
    /// bytes typed escaped, and the decoded text typed plain.
    #[test]
    fn name_equals_in_a_reinterpreting_pane_matches_bytes_and_text() {
        let latin = entry(&corpus("latin1_e_acute"), EntryKind::File, Some(1), 1);
        let enc = Some(norte_encoding::name_reinterpret_cycle()[4]);
        assert_eq!(enc.map(|e| e.label()), Some("windows-1252"));
        let q = |line: &str| CompiledQuery::new(&query(line), enc).expect("compile");
        assert!(matches(&latin, &q(r"sel name=\xe9"), NOW));
        assert!(matches(&latin, &q("sel name=é"), NOW));
        assert!(!matches(&latin, &q("sel name=e"), NOW));
    }

    fn corpus(id: &str) -> Vec<u8> {
        norte_testkit::corpus::hostile_names()
            .into_iter()
            .find(|n| n.id == id)
            .unwrap_or_else(|| panic!("fixture {id}"))
            .bytes
    }

    fn cd_in(listing: &[Entry], enc: Option<NameEncoding>, line: &str) -> Vec<u8> {
        let here = dir();
        let cx = Context {
            here: &here,
            other: None,
            listing,
            enc,
        };
        let order = norte_orders::parse(line).expect("parse");
        let [Step::Cd { to }] = &steps(&order, &cx).expect("steps")[..] else {
            panic!("{line}")
        };
        assert_eq!(to.parent(), Some(here));
        to.file_name().expect("name").as_bytes().to_vec()
    }

    /// `cd café` typed composed reaches the folder stored decomposed: the
    /// comparison is in NFC, the path keeps the folder's own bytes.
    #[test]
    fn a_typed_name_finds_its_nfd_folder_and_keeps_its_bytes() {
        let nfd = corpus("nfd_e_acute");
        let nfc = corpus("nfc_e_acute");
        let only_nfd = [entry(&nfd, EntryKind::Dir, None, 1)];
        let typed = format!("cd {}", String::from_utf8(nfc.clone()).expect("utf8"));
        assert_eq!(cd_in(&only_nfd, None, &typed), nfd);
        // Both exist (two files on ext4): the exact bytes win, never the twin.
        let both = [
            entry(&nfd, EntryKind::Dir, None, 1),
            entry(&nfc, EntryKind::Dir, None, 1),
        ];
        assert_eq!(cd_in(&both, None, &typed), nfc);
        // Not listed: the typed bytes, as typed.
        assert_eq!(cd_in(&[], None, &typed), nfc);
    }

    /// NFC folds a canonical singleton (KELVIN SIGN) into a letter a reader
    /// sees as another: typing `K.txt` never reaches the Kelvin folder.
    #[test]
    fn a_typed_name_never_reaches_a_singleton_twin() {
        for (twin, typed) in [
            ("singleton_kelvin_sign", "ascii_capital_k"),
            (
                "singleton_kelvin_sign_invalid_tail",
                "ascii_capital_k_invalid_tail",
            ),
        ] {
            let listing = [entry(&corpus(twin), EntryKind::Dir, None, 1)];
            let typed = corpus(typed);
            let line = format!("cd {}", norte_orders::escape_name(&typed));
            assert_eq!(cd_in(&listing, None, &line), typed, "{twin}");
        }
    }

    /// A pane reading its names as cp866 matches what it paints: typing
    /// `Папка` reaches the folder whose bytes are cp866.
    #[test]
    fn a_typed_name_honours_the_panes_name_encoding() {
        let papka = corpus("cp866_papka");
        let listing = [entry(&papka, EntryKind::Dir, None, 1)];
        let enc = Some(norte_encoding::name_reinterpret_cycle()[1]);
        assert_eq!(enc.map(|e| e.label()), Some("IBM866"));
        assert_eq!(cd_in(&listing, enc, "cd Папка/"), papka);
        assert_eq!(
            cd_in(&listing, None, "cd Папка"),
            "Папка".as_bytes(),
            "without the encoding, no reinterpretation"
        );
    }

    #[test]
    fn select_query_is_undone_by_restore_previous_marks() {
        let mut p = pane(vec![file("a.pdf", 1, 1), file("b.txt", 1, 1)]);
        p.mark_all();
        p.select_query(&query("sel ext:pdf"), NOW).expect("sel");
        assert_eq!(p.restore_previous_marks(), Some(2));
    }
}
