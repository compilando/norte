//! Presentation of a directory comparison — the diff pane's model, pure and
//! testable without a terminal (hard rule 7).
//!
//! The engine (`norte-compare`) answers with
//! [`CompareRow`]s and the core batches them onto the wire; everything this
//! module adds is how a HUMAN reads one. Three decisions live here and not in
//! either frontend:
//!
//! * **A glyph per verdict AND a glyph per confidence** (spec §17). `Same` at
//!   `Certain` and `Same` at `Probable` are different answers — the first
//!   proved by a hash or a size, the second guessed from a date — and a reader
//!   who cannot see colour must still be able to tell them apart. Two columns
//!   of plain ASCII, never a colour on its own.
//! * **Selection anchored to [`CompareRow::id`]**, never to a visible index.
//!   A filter hides rows; it must not move what is under the cursor, and spec
//!   2 inherits this selection as-is to seed the synchronisation plan.
//! * **An ACTIVE SIDE, chosen by the user**, never inferred from the row. The
//!   ordinary file operations act on it, and a row with nothing on that side
//!   answers [`None`] rather than quietly acting on the other one — guessing
//!   is not a feature on a delete.
//!
//! Names travel as BYTES all the way to [`cells_for`], which masks them
//! through the same [`display_name_with`](crate::display_name_with) every
//! listing uses (rule 1): lossy, and MARKED as such.

use norte_i18n::{Lang, t_in};
use norte_proto::VPath;
use norte_proto::methods::{
    CompareConfidence, CompareCriterion, CompareReason, CompareRow, CompareVerdict, Side,
};

/// The five buckets the filter keys toggle.
///
/// A partition of [`CompareVerdict`], not a selection of it: every verdict
/// belongs to exactly one, including the forward-compatible
/// [`CompareVerdict::Unknown`] a newer daemon can send. A verdict outside the
/// set would be a row no filter could hide and no legend could explain.
///
/// ```
/// use norte_frontend::compare::Category;
/// use norte_proto::methods::CompareVerdict;
///
/// assert_eq!(Category::of(CompareVerdict::OnlyLeft), Category::OnlyLeft);
/// // `TypeMismatch`, `Ambiguous` and `Error` are all "something is wrong here".
/// assert_eq!(Category::of(CompareVerdict::TypeMismatch), Category::Problems);
/// assert_eq!(Category::of(CompareVerdict::Unknown), Category::Problems);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Category {
    /// The two sides agree, at whatever confidence the criterion earned.
    Same,
    /// The two sides disagree.
    Different,
    /// Present on the left only.
    OnlyLeft,
    /// Present on the right only.
    OnlyRight,
    /// Anything the comparison could not answer cleanly: a type mismatch, an
    /// ambiguous pairing, an error row, or a verdict from a newer peer.
    Problems,
}

/// Every [`Category`], in the order the filter keys and the legend use.
///
/// ```
/// use norte_frontend::compare::{CATEGORIES, Category};
/// assert_eq!(CATEGORIES[0], Category::Same);
/// assert_eq!(CATEGORIES.len(), 5);
/// ```
pub const CATEGORIES: [Category; 5] = [
    Category::Same,
    Category::Different,
    Category::OnlyLeft,
    Category::OnlyRight,
    Category::Problems,
];

impl Category {
    /// Which bucket `verdict` falls in.
    ///
    /// ```
    /// use norte_frontend::compare::Category;
    /// use norte_proto::methods::CompareVerdict;
    /// assert_eq!(Category::of(CompareVerdict::Same), Category::Same);
    /// ```
    #[must_use]
    pub fn of(verdict: CompareVerdict) -> Self {
        match verdict {
            CompareVerdict::Same => Self::Same,
            CompareVerdict::Different => Self::Different,
            CompareVerdict::OnlyLeft => Self::OnlyLeft,
            CompareVerdict::OnlyRight => Self::OnlyRight,
            // `TypeMismatch`, `Ambiguous`, `Error` and every verdict a newer
            // daemon may invent: all "look at this one yourself".
            _ => Self::Problems,
        }
    }

    /// Stable id, for a keymap or a config to name it by.
    ///
    /// ```
    /// use norte_frontend::compare::Category;
    /// assert_eq!(Category::OnlyLeft.id(), "only-left");
    /// ```
    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Self::Same => "same",
            Self::Different => "different",
            Self::OnlyLeft => "only-left",
            Self::OnlyRight => "only-right",
            Self::Problems => "problems",
        }
    }

    /// The reader's word for it, localised.
    ///
    /// ```
    /// use norte_frontend::compare::Category;
    /// use norte_i18n::Lang;
    /// assert!(!Category::Problems.label(Lang::Es).starts_with("compare-"));
    /// ```
    #[must_use]
    pub fn label(self, lang: Lang) -> String {
        t_in(lang, &format!("compare-filter-{}", self.id()))
    }
}

/// The two glyphs a row paints: what was decided, and how sure that is.
///
/// Deliberately a pair and not one symbol. `Same` is not an answer on its own
/// — `Same`/`Certain` came from a hash or a differing size and `Same`/`Unknown`
/// from a provider that could not say — and collapsing them into one cell is
/// exactly the distinction spec §17 forbids losing.
///
/// ```
/// use norte_frontend::compare::{Glyphs, confidence_glyph, verdict_glyph};
/// use norte_proto::methods::{CompareConfidence, CompareVerdict};
///
/// let certain = Glyphs {
///     verdict: verdict_glyph(CompareVerdict::Same),
///     confidence: confidence_glyph(CompareConfidence::Certain),
/// };
/// let probable = Glyphs {
///     confidence: confidence_glyph(CompareConfidence::Probable),
///     ..certain
/// };
/// assert_ne!(certain, probable);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Glyphs {
    /// What the comparison concluded.
    pub verdict: char,
    /// What that conclusion is worth.
    pub confidence: char,
}

/// Both glyphs for one row.
///
/// ```
/// use norte_frontend::compare::glyphs;
/// use norte_proto::methods::{
///     CompareConfidence, CompareCriterion, CompareRow, CompareVerdict,
/// };
///
/// let row = CompareRow {
///     id: 1,
///     left: None,
///     right: None,
///     verdict: CompareVerdict::Same,
///     criterion: CompareCriterion::Hash,
///     confidence: CompareConfidence::Certain,
///     newer: None,
///     reason: None,
///     side: None,
/// };
/// assert_eq!(glyphs(&row).verdict, '=');
/// ```
#[must_use]
pub fn glyphs(row: &CompareRow) -> Glyphs {
    Glyphs {
        verdict: verdict_glyph(row.verdict),
        confidence: confidence_glyph(row.confidence),
    }
}

/// The glyph for a verdict.
///
/// ASCII on purpose. A terminal that cannot render a box-drawing character
/// prints a replacement box, and a legend that says "▲ means only on the left"
/// is worthless there — whereas `<` is legible on a VT100, in a pipe, and to a
/// screen reader.
///
/// ```
/// use norte_frontend::compare::verdict_glyph;
/// use norte_proto::methods::CompareVerdict;
/// assert_eq!(verdict_glyph(CompareVerdict::OnlyLeft), '<');
/// assert_eq!(verdict_glyph(CompareVerdict::OnlyRight), '>');
/// ```
#[must_use]
pub fn verdict_glyph(verdict: CompareVerdict) -> char {
    match verdict {
        CompareVerdict::Same => '=',
        CompareVerdict::Different => '#',
        CompareVerdict::OnlyLeft => '<',
        CompareVerdict::OnlyRight => '>',
        CompareVerdict::TypeMismatch => 'T',
        CompareVerdict::Ambiguous => 'A',
        CompareVerdict::Error => 'E',
        // A verdict from a newer daemon. It has a NAME this build does not
        // know, so the glyph says "something is here that I cannot read"
        // rather than borrowing another verdict's mark.
        _ => '?',
    }
}

/// The glyph for a confidence.
///
/// ```
/// use norte_frontend::compare::confidence_glyph;
/// use norte_proto::methods::CompareConfidence;
/// // "proved", "suggested", "the provider cannot say".
/// assert_eq!(confidence_glyph(CompareConfidence::Certain), '!');
/// assert_eq!(confidence_glyph(CompareConfidence::Probable), '~');
/// assert_eq!(confidence_glyph(CompareConfidence::Unknown), '?');
/// ```
#[must_use]
pub fn confidence_glyph(confidence: CompareConfidence) -> char {
    match confidence {
        CompareConfidence::Certain => '!',
        CompareConfidence::Probable => '~',
        CompareConfidence::Unknown => '?',
        // `Unrecognised` is NOT `Unknown` (ADR 0048): the first is a newer
        // peer's vocabulary, the second an honest "cannot say". Same reason
        // they have different names on the wire, they get different marks.
        _ => '-',
    }
}

/// The reader's word for a verdict.
///
/// ```
/// use norte_frontend::compare::verdict_label;
/// use norte_i18n::Lang;
/// use norte_proto::methods::CompareVerdict;
/// assert!(!verdict_label(CompareVerdict::Same, Lang::En).is_empty());
/// ```
#[must_use]
pub fn verdict_label(verdict: CompareVerdict, lang: Lang) -> String {
    let id = match verdict {
        CompareVerdict::Same => "same",
        CompareVerdict::Different => "different",
        CompareVerdict::OnlyLeft => "only-left",
        CompareVerdict::OnlyRight => "only-right",
        CompareVerdict::TypeMismatch => "type-mismatch",
        CompareVerdict::Ambiguous => "ambiguous",
        CompareVerdict::Error => "error",
        _ => "unknown",
    };
    t_in(lang, &format!("compare-verdict-{id}"))
}

/// The reader's word for a confidence.
///
/// ```
/// use norte_frontend::compare::confidence_label;
/// use norte_i18n::Lang;
/// use norte_proto::methods::CompareConfidence;
/// assert!(!confidence_label(CompareConfidence::Probable, Lang::Es).is_empty());
/// ```
#[must_use]
pub fn confidence_label(confidence: CompareConfidence, lang: Lang) -> String {
    let id = match confidence {
        CompareConfidence::Certain => "certain",
        CompareConfidence::Probable => "probable",
        CompareConfidence::Unknown => "unknown",
        _ => "unrecognised",
    };
    t_in(lang, &format!("compare-confidence-{id}"))
}

/// The reader's word for the criterion that decided a row.
///
/// ```
/// use norte_frontend::compare::criterion_label;
/// use norte_i18n::Lang;
/// use norte_proto::methods::CompareCriterion;
/// assert!(!criterion_label(CompareCriterion::Hash, Lang::En).is_empty());
/// ```
#[must_use]
pub fn criterion_label(criterion: CompareCriterion, lang: Lang) -> String {
    let id = match criterion {
        CompareCriterion::Presence => "presence",
        CompareCriterion::Kind => "kind",
        CompareCriterion::LinkTarget => "link-target",
        CompareCriterion::Size => "size",
        CompareCriterion::Mtime => "mtime",
        CompareCriterion::Hash => "hash",
        _ => "unknown",
    };
    t_in(lang, &format!("compare-criterion-{id}"))
}

/// The reader's word for why a row is `Ambiguous` or `Error`.
///
/// ```
/// use norte_frontend::compare::reason_label;
/// use norte_i18n::Lang;
/// use norte_proto::methods::CompareReason;
/// assert!(!reason_label(CompareReason::DirTooLarge, Lang::En).is_empty());
/// ```
#[must_use]
pub fn reason_label(reason: CompareReason, lang: Lang) -> String {
    let id = match reason {
        CompareReason::CaseFold => "case-fold",
        CompareReason::Normalization => "normalization",
        CompareReason::Unreadable => "unreadable",
        CompareReason::DirTooLarge => "dir-too-large",
        CompareReason::ReadFailed => "read-failed",
        _ => "unknown",
    };
    t_in(lang, &format!("compare-reason-{id}"))
}

/// The reader's word for a side.
///
/// ```
/// use norte_frontend::compare::side_label;
/// use norte_i18n::Lang;
/// use norte_proto::methods::Side;
/// assert!(!side_label(Side::Left, Lang::En).is_empty());
/// ```
#[must_use]
pub fn side_label(side: Side, lang: Lang) -> String {
    let id = match side {
        Side::Left => "left",
        Side::Right => "right",
        // A side a newer peer named. `Side` is a CLOSED enum plus its
        // forward-compat variant, so this is exhaustive by name rather than
        // by wildcard — the day a third real side exists, this stops
        // compiling instead of silently calling it "unknown".
        Side::Unknown => "unknown",
    };
    t_in(lang, &format!("compare-side-{id}"))
}

/// One face of a row: what a pane column would have painted for that entry.
///
/// The name is already masked and already flagged — the frontend applies its
/// badge, exactly as it does for a listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowFace {
    /// Display form of the entry's name: lossy, and safe to paint.
    pub name: String,
    /// The name's ORIGINAL bytes. A frontend styles from these and paints
    /// [`RowFace::name`] — the two are not interchangeable: a theme matches
    /// an extension against the real bytes, and matching against the masked
    /// form would give a non-UTF-8 file one colour in a listing and another
    /// in the comparison of that same listing (rule 1).
    pub raw_name: Vec<u8>,
    /// The name was altered to be painted (spec §6): badge it.
    pub hostile: bool,
    /// Size in bytes, when the provider knows it.
    pub size: Option<u64>,
    /// Modification time, when the provider knows it.
    pub mtime_ms: Option<i64>,
    /// Directories sort and paint differently from files.
    pub kind: norte_proto::EntryKind,
}

/// A whole row, ready to paint: two faces and the two glyphs between them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowCells {
    /// Verdict and confidence marks.
    pub glyphs: Glyphs,
    /// The left face, absent for an orphan on the right (and for an `Error`
    /// row that has no entry to show at all).
    pub left: Option<RowFace>,
    /// The right face.
    pub right: Option<RowFace>,
}

fn face(entry: &norte_proto::Entry, reinterpret: Option<norte_encoding::NameEncoding>) -> RowFace {
    let bytes = entry
        .path
        .file_name()
        .map_or_else(Vec::new, |s| s.as_bytes().to_vec());
    let (name, hostile) = crate::display_name_with(&bytes, reinterpret);
    RowFace {
        name,
        raw_name: bytes,
        hostile,
        size: entry.size,
        mtime_ms: entry.mtime_ms,
        kind: entry.kind,
    }
}

/// Everything a painter needs for one row, with both names already masked.
///
/// One name-encoding override PER SIDE (#57), passed through unchanged so a
/// comparison reads exactly the way the two listings it came from do. Two and
/// not one, because the two panes are two locations and can carry different
/// overrides: a reader who pressed `Alt+E` to make a CP1251 share legible must
/// not get `����.txt` back the moment they compare it.
///
/// ```
/// use norte_frontend::compare::cells_for;
/// use norte_proto::methods::{
///     CompareConfidence, CompareCriterion, CompareRow, CompareVerdict,
/// };
/// use norte_proto::{Entry, EntryKind, VPath};
///
/// let row = CompareRow {
///     id: 1,
///     left: Some(Entry {
///         path: VPath::parse("file:///a/b.txt").expect("path"),
///         kind: EntryKind::File,
///         size: Some(3),
///         mtime_ms: None,
///         attrs: std::collections::BTreeMap::new(),
///     }),
///     right: None,
///     verdict: CompareVerdict::OnlyLeft,
///     criterion: CompareCriterion::Presence,
///     confidence: CompareConfidence::Certain,
///     newer: None,
///     reason: None,
///     side: None,
/// };
/// let cells = cells_for(&row, None, None);
/// assert_eq!(cells.left.expect("left face").name, "b.txt");
/// assert!(cells.right.is_none());
/// ```
#[must_use]
pub fn cells_for(
    row: &CompareRow,
    left_reinterpret: Option<norte_encoding::NameEncoding>,
    right_reinterpret: Option<norte_encoding::NameEncoding>,
) -> RowCells {
    RowCells {
        glyphs: glyphs(row),
        left: row.left.as_ref().map(|e| face(e, left_reinterpret)),
        right: row.right.as_ref().map(|e| face(e, right_reinterpret)),
    }
}

/// The diff pane's model: the rows that have arrived, which categories are
/// hidden, which row is selected, and which side the file operations act on.
///
/// Rows only ever grow — [`ComparePane::extend`] appends what the stream
/// delivers and nothing removes them — because the engine emits every row
/// FINAL: no row is ever corrected later, so there is nothing to reconcile and
/// no reason to drop one.
///
/// # This collection is UNBOUNDED, and that is a decision
///
/// One row per paired name over the whole tree, and nothing caps the total.
/// `COMPARE_MAX_DIR_ENTRIES` caps a single DIRECTORY, not the walk, and the
/// TUI asks with `max_depth: None`. Each row carries two whole `Entry` values,
/// each with a `VPath` that repeats its parent segments — on the order of a
/// kilobyte and a dozen allocations per row, so a comparison of two trees of
/// half a million paired names is hundreds of megabytes in the client.
///
/// The alternative — a cap with a `Truncated` state, the way live search caps
/// at `SEARCH_MAX_HITS` — was rejected: a search that stops at the ten
/// thousandth hit has still answered "here are some", while a comparison that
/// stops at row N has not answered "are these two trees the same?" at all.
/// Completeness is the entire value of this answer, and a cap would quietly
/// destroy it. Search can also cap SERVER-side (`max_hits` is a request
/// parameter); `fs.compare` has no such field, so a client-side cap would
/// leave the daemon walking a tree whose rows nobody keeps.
///
/// **Cancelling the task is therefore the only brake**, and the pane keeps
/// what arrived because a cancelled comparison's rows are still true.
/// Anything stronger belongs on the WIRE, as a limit the engine honours, and
/// not here.
///
/// ```
/// use norte_frontend::compare::{Category, ComparePane};
/// let pane = ComparePane::new();
/// assert!(pane.is_empty());
/// assert!(!pane.is_hidden(Category::Same), "nothing is filtered out to start");
/// ```
#[derive(Debug)]
pub struct ComparePane {
    /// Every row that arrived, in stream order (which is `id` order: the
    /// engine numbers them monotonically).
    rows: Vec<CompareRow>,
    /// The categories the reader has toggled OFF. A `Vec` of at most five,
    /// which is cheaper to scan than a set is to hash.
    hidden: Vec<Category>,
    /// How many rows fell in each [`CATEGORIES`] bucket, kept up to date by
    /// [`ComparePane::extend`].
    ///
    /// Cached rather than counted on demand because the footer prints all
    /// five EVERY FRAME, and counting them meant five full scans of a vector
    /// that can hold a million rows — per frame, ten times a second, while
    /// the walk is still feeding it.
    counts: [usize; CATEGORIES.len()],
    /// The selected row's `id` — never an index. A filter changes which rows
    /// are on screen and must not change which one is selected.
    selected: Option<u64>,
    /// The rows the reader MARKED, by `id`, for the same reason `selected` is
    /// an id: a filter must not change what was picked.
    ///
    /// Distinct from `selected`, which is the cursor. This is the set that
    /// seeds `SyncPlanParams::include`, so an empty set means "the whole
    /// tree" — the wire's own convention, where the field is absent rather
    /// than an empty list (`Some(vec![])` is a plan of nothing).
    ///
    /// A `BTreeSet` and not a `Vec`: `include` is capped at
    /// `SYNC_MAX_INCLUDE`, so the set is small, and ordered ids make the
    /// request byte-stable for the same picks — which is what keeps two
    /// identical selections producing one `plan_hash`.
    marked: std::collections::BTreeSet<u64>,
    /// The side the ordinary file operations act on. Left is the pane that
    /// launched the comparison.
    active: Side,
}

impl Default for ComparePane {
    fn default() -> Self {
        Self::new()
    }
}

impl ComparePane {
    /// An empty pane: no rows, no filters, nothing selected, left side active.
    ///
    /// ```
    /// use norte_frontend::compare::ComparePane;
    /// use norte_proto::methods::Side;
    /// assert_eq!(ComparePane::new().active_side(), Side::Left);
    /// ```
    #[must_use]
    pub fn new() -> Self {
        Self {
            rows: Vec::new(),
            hidden: Vec::new(),
            counts: [0; CATEGORIES.len()],
            selected: None,
            marked: std::collections::BTreeSet::new(),
            active: Side::Left,
        }
    }

    /// Appends a batch, and selects the first row if nothing is selected yet.
    ///
    /// ```
    /// use norte_frontend::compare::ComparePane;
    /// let mut pane = ComparePane::new();
    /// pane.extend(Vec::new());
    /// assert_eq!(pane.selected_id(), None, "an empty batch selects nothing");
    /// ```
    pub fn extend(&mut self, rows: impl IntoIterator<Item = CompareRow>) {
        for row in rows {
            let category = Category::of(row.verdict);
            if let Some(i) = CATEGORIES.iter().position(|c| *c == category) {
                self.counts[i] += 1;
            }
            self.rows.push(row);
        }
        if self.selected.is_none() {
            let first = self.visible().next().map(|r| r.id);
            self.selected = first;
        }
    }

    /// Every row that arrived, filters ignored.
    #[must_use]
    pub fn rows(&self) -> &[CompareRow] {
        &self.rows
    }

    /// How many rows arrived, filters ignored.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Whether no row has arrived yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// How many rows fell in `category`, filters ignored — the status bar
    /// counts what ARRIVED, not what is on screen.
    #[must_use]
    pub fn count_of(&self, category: Category) -> usize {
        CATEGORIES
            .iter()
            .position(|c| *c == category)
            .and_then(|i| self.counts.get(i).copied())
            .unwrap_or(0)
    }

    /// The rows a filter has not hidden, in arrival order.
    pub fn visible(&self) -> impl Iterator<Item = &CompareRow> + '_ {
        self.rows
            .iter()
            .filter(|r| !self.hidden.contains(&Category::of(r.verdict)))
    }

    /// The ids of [`ComparePane::visible`].
    #[must_use]
    pub fn visible_ids(&self) -> Vec<u64> {
        self.visible().map(|r| r.id).collect()
    }

    /// How many rows are on screen.
    #[must_use]
    pub fn visible_len(&self) -> usize {
        self.visible().count()
    }

    /// Whether `category` is currently filtered out.
    #[must_use]
    pub fn is_hidden(&self, category: Category) -> bool {
        self.hidden.contains(&category)
    }

    /// Shows or hides a whole category. The selection is untouched, by
    /// design: it is anchored to an id, and hiding the row it names does not
    /// unname it.
    pub fn toggle_filter(&mut self, category: Category) {
        if let Some(i) = self.hidden.iter().position(|c| *c == category) {
            self.hidden.remove(i);
        } else {
            self.hidden.push(category);
        }
    }

    /// Selects the row with this id. A no-op if no such row arrived — the
    /// alternative is a `selected_id` naming a row that does not exist.
    pub fn select(&mut self, id: u64) {
        if self.rows.iter().any(|r| r.id == id) {
            self.selected = Some(id);
        }
    }

    /// The selected row's id, whether or not a filter is hiding it.
    #[must_use]
    pub fn selected_id(&self) -> Option<u64> {
        self.selected
    }

    /// The selected row.
    #[must_use]
    pub fn selected_row(&self) -> Option<&CompareRow> {
        let id = self.selected?;
        self.rows.iter().find(|r| r.id == id)
    }

    /// Marks or unmarks a row, by id. A row that never arrived is ignored,
    /// exactly as [`ComparePane::select`] ignores it, and for the same reason:
    /// a mark on a row that does not exist would name nothing when the
    /// selection becomes a `SyncPlanParams::include` list.
    ///
    /// ```
    /// use norte_frontend::compare::ComparePane;
    /// let mut pane = ComparePane::new();
    /// pane.toggle_mark(7);
    /// assert!(!pane.is_marked(7), "no such row arrived");
    /// ```
    pub fn toggle_mark(&mut self, id: u64) {
        if !self.rows.iter().any(|r| r.id == id) {
            return;
        }
        if !self.marked.remove(&id) {
            self.marked.insert(id);
        }
    }

    /// Is this row marked?
    #[must_use]
    pub fn is_marked(&self, id: u64) -> bool {
        self.marked.contains(&id)
    }

    /// How many rows are marked. Includes rows a filter is hiding — hiding a
    /// category does not unpick what was picked, and a count that shrank when
    /// the reader pressed `2` would be lying about what is about to be
    /// synchronised.
    #[must_use]
    pub fn marked_len(&self) -> usize {
        self.marked.len()
    }

    /// The marked rows, in id order (which is stream order).
    ///
    /// Borrowed from `rows` rather than cloned: the caller only ever reads a
    /// path out of them.
    #[must_use]
    pub fn marked_rows(&self) -> Vec<&CompareRow> {
        self.rows
            .iter()
            .filter(|r| self.marked.contains(&r.id))
            .collect()
    }

    /// Where the selected row sits among the VISIBLE ones — the index a list
    /// widget highlights. `None` when a filter is hiding it, which is the
    /// honest answer: there is no cell to highlight.
    #[must_use]
    pub fn visible_index(&self) -> Option<usize> {
        let id = self.selected?;
        self.visible().position(|r| r.id == id)
    }

    /// Moves the cursor `delta` rows through what is VISIBLE, clamped at both
    /// ends.
    ///
    /// When the selected row is hidden (or there is none) the move starts from
    /// the top of what is left rather than from an index into a list that no
    /// longer contains it — the alternative is a `down` that appears to do
    /// nothing.
    pub fn move_by(&mut self, delta: isize) {
        // Walked, never collected: materialising every visible id was an
        // eight-megabyte allocation per `Down` on a million-row comparison.
        let total = self.visible_len();
        if total == 0 {
            return;
        }
        let from = self
            .visible_index()
            .map_or(0isize, |i| isize::try_from(i).unwrap_or(isize::MAX));
        let last = isize::try_from(total - 1).unwrap_or(isize::MAX);
        let to = from.saturating_add(delta).clamp(0, last);
        let index = usize::try_from(to).unwrap_or(0);
        let id = self.visible().nth(index).map(|r| r.id);
        if id.is_some() {
            self.selected = id;
        }
    }

    /// Selects the first visible row.
    pub fn select_first(&mut self) {
        // A no-op with everything filtered out: `toggle_filter` promises a
        // filter never disturbs the selection, and clearing the anchor here
        // because nothing is on screen would break that promise from the
        // other side.
        let first = self.visible().next().map(|r| r.id);
        if first.is_some() {
            self.selected = first;
        }
    }

    /// Selects the last visible row.
    pub fn select_last(&mut self) {
        let last = self.visible().last().map(|r| r.id);
        if last.is_some() {
            self.selected = last;
        }
    }

    /// The side the file operations act on.
    #[must_use]
    pub fn active_side(&self) -> Side {
        self.active
    }

    /// Swaps the active side.
    pub fn swap_active_side(&mut self) {
        self.active = match self.active {
            Side::Right => Side::Left,
            // `Left` and anything a newer peer could name both resolve to a
            // concrete side rather than staying unusable.
            _ => Side::Right,
        };
    }

    /// The entry the selected row carries on the ACTIVE side.
    ///
    /// `None` when that side is empty — an orphan seen from the wrong side, or
    /// an `Error` row that carries no entry at all (C6 finding 7). The caller
    /// must not fall back to the other side.
    #[must_use]
    pub fn target_entry(&self) -> Option<&norte_proto::Entry> {
        let row = self.selected_row()?;
        match self.active {
            Side::Right => row.right.as_ref(),
            _ => row.left.as_ref(),
        }
    }

    /// The path of [`ComparePane::target_entry`].
    #[must_use]
    pub fn target_path(&self) -> Option<&VPath> {
        self.target_entry().map(|e| &e.path)
    }

    /// The DIRECTORY that opening the selected row should navigate to: the
    /// row's own path when it is a directory, its parent when it is a file.
    ///
    /// This is how an orphan directory gets expanded — the walk reports it as
    /// one row and never enumerates it, so the way to see inside is to go
    /// there. Lives here rather than in a frontend (rule 7) because it is the
    /// same rule in the TUI and the GUI, and `None` for an empty active side
    /// still means "do not fall back to the other one".
    ///
    /// ```
    /// use norte_frontend::compare::ComparePane;
    /// // Nothing selected, nowhere to go.
    /// assert_eq!(ComparePane::new().navigation_target(), None);
    /// ```
    #[must_use]
    pub fn navigation_target(&self) -> Option<VPath> {
        let entry = self.target_entry()?;
        if entry.kind == norte_proto::EntryKind::Dir {
            Some(entry.path.clone())
        } else {
            entry.path.parent()
        }
    }
}

/// Estado de presentación de una comparación de directorios (`Shift+F2`,
/// 2026-08-11-directory-comparison.md): el run loop lo refleja en
/// [`CompareView::state`] para que la barra elija la variante
/// `compare-status-*`.
///
/// Mismo molde que el `SearchState` de una búsqueda viva, con UNA variante de
/// más y la razón por la que existe: en `fs.compare` el cierre del canal de
/// filas NO significa «ya llegaron todas». La bomba de filas y la del
/// snapshot terminal son tasks
/// independientes, así que al acabarse el flujo se compara lo recibido contra
/// `TaskProgress::entries_done` — y si falta algo, [`CompareState::Incomplete`]
/// lo DICE en vez de pintar «hecho» sobre una respuesta a medias. En una
/// comparación, lo completa que está la respuesta *es* la respuesta.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CompareState {
    /// El walk sigue emitiendo filas.
    #[default]
    Running,
    /// Terminó y llegaron todas las filas que la task contó.
    Done,
    /// Terminó, pero llegaron MENOS filas de las que la task contó: se perdió
    /// algún lote por el camino.
    Incomplete,
    /// El usuario canceló (las filas ya llegadas se conservan).
    Cancelled,
    /// La task falló (el error va por la barra).
    Failed,
}

/// El panel de diferencias abierto (`Shift+F2`): el modelo puro que vive en
/// `norte-frontend` más lo que la TUI necesita para pintarlo y para decir
/// cómo acabó.
///
/// El modelo (filas, filtros, selección por id, lado activo) NO está aquí a
/// propósito (regla dura 7): vive en [`norte_frontend::compare::ComparePane`],
/// donde se testea sin terminal, y esta struct solo le añade el estado del run
/// y las dos raíces que la cabecera pinta.
#[derive(Debug)]
pub struct CompareView {
    /// Filas, filtros, selección y lado activo.
    pub pane: ComparePane,
    /// Cómo va (o cómo acabó) la comparación.
    pub state: CompareState,
    /// Categoría del error de una comparación que FALLÓ, ya localizada y
    /// saneada. Se pinta de forma PERSISTENTE, igual que el `search_error`
    /// de una búsqueda viva: un fallo no puede degradar a «hecho» en la
    /// siguiente tecla.
    pub error: Option<String>,
    /// Cuántas filas contó la task (`TaskProgress::entries_done`) cuando se
    /// cerró el flujo. Solo significativo con [`CompareState::Incomplete`],
    /// que es el único caso en el que difiere de las filas que hay.
    pub rows_expected: u64,
    /// Raíz izquierda: el pane que lanzó la comparación.
    pub left_root: VPath,
    /// Raíz derecha.
    pub right_root: VPath,
    /// Índice del pane que ES el lado izquierdo — el que lanzó la
    /// comparación, que no tiene por qué ser `panes[0]`.
    ///
    /// Se congela al abrir y decide a QUÉ pane navega el `Enter` de una fila:
    /// al que le corresponde al lado ACTIVO. Sin esto el `Enter` mandaba
    /// siempre al pane con foco, así que mirando el lado derecho el lector
    /// perdía su directorio izquierdo para ir a ver el derecho — lo cazó el
    /// arnés de tmux, y ninguna aserción del modelo podía verlo.
    pub left_pane: usize,
    /// Ya se pidió cancelar esta comparación (el primer `Esc`).
    ///
    /// El segundo `Esc` cierra el panel PASE LO QUE PASE con la Task. Sin
    /// esto el cierre dependía de que el canal de filas llegara a cerrarse, y
    /// hay formas de que no lo haga —un daemon caído, un provider colgado en
    /// una NFS muerta—, con lo que el lector se quedaba encerrado en la única
    /// pantalla de norte de la que no se sale (review BLOCKER-1).
    pub cancel_requested: bool,
    /// Reinterpretación de nombres (#57) de CADA lado, congelada al abrir.
    ///
    /// Dos y no una: los dos panes son dos ubicaciones y pueden llevar
    /// overrides distintos. Sin esto, un lector que había pulsado `Alt+E`
    /// para leer un share CP1251 recuperaba `????.txt` en cuanto lo comparaba
    /// (review MAJOR-3).
    pub left_encoding: Option<norte_encoding::NameEncoding>,
    /// La del lado derecho.
    pub right_encoding: Option<norte_encoding::NameEncoding>,
}

impl CompareView {
    /// Un panel recién abierto sobre estas dos raíces, sin filas todavía.
    #[must_use]
    pub fn new(
        left_root: VPath,
        right_root: VPath,
        left_pane: usize,
        left_encoding: Option<norte_encoding::NameEncoding>,
        right_encoding: Option<norte_encoding::NameEncoding>,
    ) -> Self {
        Self {
            pane: ComparePane::new(),
            state: CompareState::Running,
            error: None,
            rows_expected: 0,
            left_root,
            right_root,
            left_pane,
            cancel_requested: false,
            left_encoding,
            right_encoding,
        }
    }

    /// Se cerró el canal de filas: decide el estado terminal a partir de lo
    /// que la task CONTÓ (`entries_done`, de `TaskProgress`) contra lo que
    /// realmente LLEGÓ (`rows_received`).
    ///
    /// Esto y no el cierre del canal es lo que dice si la comparación
    /// terminó de verdad: la bomba de filas y la del snapshot terminal son
    /// tasks independientes (ver [`CompareState::Incomplete`]), así que un
    /// canal cerrado con menos filas de las contadas es un lote perdido, no
    /// una respuesta completa. El CLI (fase A) y la tool MCP (fase B)
    /// reimplementaron esta cuenta cada uno por su lado y los dos se
    /// equivocaron exactamente aquí — de ahí que viva en el modelo y no en
    /// cada frontend.
    ///
    /// No decide `Cancelled` ni `Failed`: esos salen directamente del
    /// `TaskState` del run, no de un conteo de filas, y quien los conoce se
    /// los asigna a [`CompareView::state`] sin pasar por aquí.
    pub fn finish(&mut self, entries_done: u64, rows_received: u64) {
        self.rows_expected = entries_done;
        self.state = if rows_received < entries_done {
            CompareState::Incomplete
        } else {
            CompareState::Done
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use norte_proto::methods::CompareConfidence::{Certain, Probable, Unknown};
    use norte_proto::methods::CompareVerdict::{Different, Error, OnlyLeft, Same};
    use norte_proto::methods::{
        CompareConfidence, CompareCriterion, CompareReason, CompareRow, CompareVerdict, Side,
    };
    use norte_proto::{Entry, EntryKind, VPath};

    fn left_path() -> VPath {
        VPath::parse("file:///left/x").expect("path")
    }

    fn right_path() -> VPath {
        VPath::parse("file:///right/x").expect("path")
    }

    fn entry(path: &VPath) -> Entry {
        Entry {
            path: path.clone(),
            kind: EntryKind::File,
            size: Some(1),
            mtime_ms: Some(0),
            attrs: std::collections::BTreeMap::new(),
        }
    }

    /// A row whose sides agree with its verdict — what the engine always
    /// emits (`CompareRow::sides_are_consistent`).
    fn row_id(id: u64, verdict: CompareVerdict) -> CompareRow {
        let (left, right) = match verdict {
            CompareVerdict::OnlyLeft => (Some(entry(&left_path())), None),
            CompareVerdict::OnlyRight => (None, Some(entry(&right_path()))),
            _ => (Some(entry(&left_path())), Some(entry(&right_path()))),
        };
        CompareRow {
            id,
            left,
            right,
            verdict,
            criterion: CompareCriterion::Presence,
            confidence: CompareConfidence::Certain,
            newer: None,
            reason: match verdict {
                CompareVerdict::Ambiguous | CompareVerdict::Error => {
                    Some(CompareReason::Unreadable)
                }
                _ => None,
            },
            side: None,
        }
    }

    fn row(verdict: CompareVerdict, confidence: CompareConfidence) -> CompareRow {
        CompareRow {
            confidence,
            ..row_id(1, verdict)
        }
    }

    fn pane_with(rows: Vec<CompareRow>) -> ComparePane {
        let mut pane = ComparePane::new();
        pane.extend(rows);
        pane
    }

    /// §17: textual cues, not colour alone. `Same/Probable` and `Same/Certain`
    /// are different answers, and a user who cannot see colour must still be
    /// able to tell them apart.
    #[test]
    fn confidence_is_visible_without_colour() {
        let certain = glyphs(&row(Same, Certain));
        let probable = glyphs(&row(Same, Probable));
        let unknown = glyphs(&row(Same, Unknown));
        assert_ne!(certain, probable);
        assert_ne!(probable, unknown);
        assert_ne!(certain, unknown);
    }

    /// The other half of the same requirement: two different VERDICTS must not
    /// share a glyph either, or the column is decoration.
    #[test]
    fn every_verdict_has_its_own_glyph() {
        let mut seen: Vec<char> = [
            CompareVerdict::Same,
            CompareVerdict::Different,
            CompareVerdict::OnlyLeft,
            CompareVerdict::OnlyRight,
            CompareVerdict::TypeMismatch,
            CompareVerdict::Ambiguous,
            CompareVerdict::Error,
            CompareVerdict::Unknown,
        ]
        .iter()
        .map(|v| verdict_glyph(*v))
        .collect();
        let before = seen.len();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(before, seen.len(), "two verdicts share a glyph: {seen:?}");
    }

    /// Filters hide rows; they never renumber them. The selection is anchored
    /// to the row id precisely so that a filter cannot move what is selected.
    #[test]
    fn filtering_hides_rows_without_disturbing_the_selection() {
        let mut pane = pane_with(vec![
            row_id(1, Same),
            row_id(2, OnlyLeft),
            row_id(3, Different),
        ]);
        pane.select(2);
        pane.toggle_filter(Category::Same);
        assert_eq!(pane.visible_ids(), vec![2, 3]);
        assert_eq!(pane.selected_id(), Some(2));
        pane.toggle_filter(Category::Same);
        assert_eq!(pane.visible_ids(), vec![1, 2, 3]);
        assert_eq!(pane.selected_id(), Some(2));
    }

    /// Actions go to the ACTIVE side, never to a side inferred from the row.
    /// On destructive operations, guessing is not a feature.
    #[test]
    fn actions_target_the_active_side_not_the_inferred_one() {
        let mut pane = pane_with(vec![row_id(1, Different)]);
        pane.select(1);
        assert_eq!(pane.target_path(), Some(&left_path()));
        pane.swap_active_side();
        assert_eq!(pane.target_path(), Some(&right_path()));
    }

    /// A row with nothing on the active side has no target — the caller gets
    /// `None` and must not fall back to the other side behind the user's back.
    #[test]
    fn an_orphan_row_has_no_target_on_the_empty_side() {
        let mut pane = pane_with(vec![row_id(1, OnlyLeft)]);
        pane.select(1);
        pane.swap_active_side();
        assert_eq!(pane.target_path(), None);
    }

    /// C6's finding 7: `Error` and `Ambiguous` are the two verdicts exempt
    /// from the sides invariant, and a directory that failed to list on BOTH
    /// sides carries NEITHER entry. `target_path` must answer `None` for that
    /// row on either side rather than index into an empty option.
    #[test]
    fn a_row_with_neither_side_has_no_target_whichever_side_is_active() {
        let orphaned_error = CompareRow {
            left: None,
            right: None,
            reason: Some(CompareReason::Unreadable),
            ..row_id(1, Error)
        };
        assert!(orphaned_error.sides_are_consistent());
        let mut pane = pane_with(vec![orphaned_error]);
        pane.select(1);
        assert_eq!(pane.target_path(), None);
        pane.swap_active_side();
        assert_eq!(pane.target_path(), None);
        assert_eq!(pane.selected_id(), Some(1), "the row is still selectable");
    }

    /// The five filter categories must cover every verdict: a verdict that
    /// belongs to no category is a row no filter can ever hide OR show.
    #[test]
    fn the_five_categories_cover_every_verdict() {
        for verdict in [
            CompareVerdict::Same,
            CompareVerdict::Different,
            CompareVerdict::OnlyLeft,
            CompareVerdict::OnlyRight,
            CompareVerdict::TypeMismatch,
            CompareVerdict::Ambiguous,
            CompareVerdict::Error,
            CompareVerdict::Unknown,
        ] {
            let category = Category::of(verdict);
            assert!(
                CATEGORIES.contains(&category),
                "{verdict:?} maps outside the filter set"
            );
            let mut pane = pane_with(vec![row_id(1, verdict)]);
            pane.toggle_filter(category);
            assert_eq!(
                pane.visible_ids(),
                Vec::<u64>::new(),
                "{verdict:?} survived its own filter"
            );
        }
    }

    /// Every word the pane paints is a Fluent id in BOTH locales. An id with
    /// no message renders as the id itself, so `compare-verdict-same` is what
    /// a reader would see — the failure mode `keymap-reason-*` already taught
    /// this repo about.
    #[test]
    fn every_label_is_translated_in_both_locales() {
        for lang in [norte_i18n::Lang::En, norte_i18n::Lang::Es] {
            for v in [
                CompareVerdict::Same,
                CompareVerdict::Different,
                CompareVerdict::OnlyLeft,
                CompareVerdict::OnlyRight,
                CompareVerdict::TypeMismatch,
                CompareVerdict::Ambiguous,
                CompareVerdict::Error,
                CompareVerdict::Unknown,
            ] {
                let s = verdict_label(v, lang);
                assert!(!s.starts_with("compare-"), "{lang:?} {v:?}: {s}");
            }
            for c in [
                CompareConfidence::Certain,
                CompareConfidence::Probable,
                CompareConfidence::Unknown,
                CompareConfidence::Unrecognised,
            ] {
                let s = confidence_label(c, lang);
                assert!(!s.starts_with("compare-"), "{lang:?} {c:?}: {s}");
            }
            for c in [
                CompareCriterion::Presence,
                CompareCriterion::Kind,
                CompareCriterion::LinkTarget,
                CompareCriterion::Size,
                CompareCriterion::Mtime,
                CompareCriterion::Hash,
                CompareCriterion::Unknown,
            ] {
                let s = criterion_label(c, lang);
                assert!(!s.starts_with("compare-"), "{lang:?} {c:?}: {s}");
            }
            for r in [
                CompareReason::CaseFold,
                CompareReason::Normalization,
                CompareReason::Unreadable,
                CompareReason::DirTooLarge,
                CompareReason::ReadFailed,
                CompareReason::Unknown,
            ] {
                let s = reason_label(r, lang);
                assert!(!s.starts_with("compare-"), "{lang:?} {r:?}: {s}");
            }
            for cat in CATEGORIES {
                let s = cat.label(lang);
                assert!(!s.starts_with("compare-"), "{lang:?} {cat:?}: {s}");
            }
            for side in [Side::Left, Side::Right, Side::Unknown] {
                let s = side_label(side, lang);
                assert!(!s.starts_with("compare-"), "{lang:?} {side:?}: {s}");
            }
        }
    }

    /// A name is bytes and the pane paints it, so it goes through the same
    /// lossy-and-MARKED path every other listing does (rule 1, spec §6).
    #[test]
    fn a_hostile_name_is_masked_and_flagged_on_both_faces() {
        let hostile = VPath::parse("file:///d")
            .expect("dir")
            .join(norte_proto::Segment::new(b"a\nb".to_vec()).expect("segment"));
        let row = CompareRow {
            left: Some(entry(&hostile)),
            right: Some(entry(&hostile)),
            ..row_id(1, Same)
        };
        let cells = cells_for(&row, None, None);
        let left = cells.left.expect("a left face");
        assert!(left.hostile, "a newline in a name must be flagged");
        assert!(
            !left.name.contains('\n'),
            "the raw byte must not reach paint"
        );
        assert!(cells.right.expect("a right face").hostile);
    }

    /// The cursor walks what is VISIBLE. Filtering the row under the cursor
    /// away must not strand the next `down` — it starts from the top of what
    /// is left rather than from an index into a list that no longer has it.
    #[test]
    fn the_cursor_walks_only_visible_rows() {
        let mut pane = pane_with(vec![row_id(1, Same), row_id(2, Same), row_id(3, OnlyLeft)]);
        pane.select(1);
        pane.move_by(1);
        assert_eq!(pane.selected_id(), Some(2));
        pane.toggle_filter(Category::Same);
        assert_eq!(pane.visible_ids(), vec![3]);
        pane.move_by(1);
        assert_eq!(
            pane.selected_id(),
            Some(3),
            "the cursor followed the filter"
        );
        pane.move_by(1);
        assert_eq!(pane.selected_id(), Some(3), "and it clamps at the end");
    }

    /// A filter that hides EVERYTHING must not quietly forget what was
    /// selected. `toggle_filter` promises a filter never disturbs the
    /// selection, and `select_first`/`select_last` used to break that promise
    /// from the other side by setting `None` when nothing was visible
    /// (reviewer MINOR).
    #[test]
    fn a_cursor_key_over_an_empty_list_keeps_the_anchor() {
        let mut pane = pane_with(vec![row_id(1, Same), row_id(2, Same)]);
        pane.select(2);
        pane.toggle_filter(Category::Same);
        assert_eq!(pane.visible_ids(), Vec::<u64>::new());
        pane.select_first();
        pane.move_by(1);
        pane.select_last();
        assert_eq!(pane.selected_id(), Some(2), "el ancla sobrevive");
        pane.toggle_filter(Category::Same);
        assert_eq!(pane.selected_id(), Some(2));
    }

    /// The counts are CACHED, so the cache and the rows must not drift: a
    /// count read from a stale cache is a footer that lies about how much of
    /// the answer a filter is hiding.
    #[test]
    fn the_cached_counts_match_a_full_recount() {
        let mut pane = ComparePane::new();
        pane.extend(vec![row_id(1, Same), row_id(2, OnlyLeft)]);
        pane.extend(vec![row_id(3, Same), row_id(4, Error)]);
        for category in CATEGORIES {
            let recuento = pane
                .rows()
                .iter()
                .filter(|r| Category::of(r.verdict) == category)
                .count();
            assert_eq!(pane.count_of(category), recuento, "{category:?}");
        }
    }

    /// `Enter` opens a DIRECTORY row where it lives and a FILE row where its
    /// parent lives — the rule that expands an orphan directory the walk
    /// reported as one row and never enumerated. Both frontends need it, so
    /// it is here and not in either of them (rule 7).
    #[test]
    fn opening_a_row_goes_to_the_directory_it_lives_in() {
        let dir = VPath::parse("file:///left/sub").expect("path");
        let mut orphan = row_id(1, OnlyLeft);
        orphan.left = Some(Entry {
            path: dir.clone(),
            kind: EntryKind::Dir,
            size: None,
            mtime_ms: None,
            attrs: std::collections::BTreeMap::new(),
        });
        let mut pane = pane_with(vec![orphan]);
        pane.select(1);
        assert_eq!(pane.navigation_target(), Some(dir), "a dir opens itself");

        let mut pane = pane_with(vec![row_id(2, Different)]);
        pane.select(2);
        assert_eq!(
            pane.navigation_target(),
            left_path().parent(),
            "a file opens its parent"
        );
        pane.swap_active_side();
        assert_eq!(pane.navigation_target(), right_path().parent());
    }

    /// A name is bytes twice over: the masked form is what gets PAINTED, and
    /// the original is what a theme matches an extension against. Losing the
    /// second gave a non-UTF-8 file one colour in a listing and another in
    /// the comparison of that same listing (reviewer MINOR).
    #[test]
    fn a_face_carries_the_original_bytes_beside_the_masked_name() {
        let raw = b"\xff\xfe.rs";
        let hostile = VPath::parse("file:///d")
            .expect("dir")
            .join(norte_proto::Segment::new(raw.to_vec()).expect("segment"));
        let row = CompareRow {
            left: Some(entry(&hostile)),
            ..row_id(1, OnlyLeft)
        };
        let face = cells_for(&row, None, None).left.expect("a left face");
        assert_eq!(face.raw_name, raw, "los bytes crudos viajan intactos");
        assert_ne!(face.name.as_bytes(), raw, "y lo pintado no son esos bytes");
    }

    /// The counts the status bar prints are counts of what ARRIVED, not of
    /// what is on screen: hiding the `Same` rows must not make them stop
    /// existing.
    #[test]
    fn the_counts_describe_every_row_not_the_visible_ones() {
        let mut pane = pane_with(vec![
            row_id(1, Same),
            row_id(2, Same),
            row_id(3, OnlyLeft),
            row_id(4, Error),
        ]);
        pane.toggle_filter(Category::Same);
        assert_eq!(pane.len(), 4);
        assert_eq!(pane.count_of(Category::Same), 2);
        assert_eq!(pane.count_of(Category::OnlyLeft), 1);
        assert_eq!(pane.count_of(Category::Problems), 1);
        assert_eq!(pane.count_of(Category::Different), 0);
    }

    /// #158: el estado del run vivía en `norte-tui`, así que la GUI habría
    /// tenido que reimplementarlo — y las dos superficies que ya lo
    /// reimplementaron (el CLI en la fase A, la tool MCP en la fase B) se
    /// equivocaron en lo mismo: dieron por completa una respuesta a la que le
    /// faltaban lotes. Aquí, y una sola vez.
    #[test]
    fn el_estado_del_run_vive_con_el_modelo() {
        let mut v = CompareView::new(
            VPath::parse("file:///a").expect("wire"),
            VPath::parse("file:///b").expect("wire"),
            0,
            None,
            None,
        );
        assert_eq!(v.state, CompareState::Running, "nace corriendo");
        assert!(v.pane.is_empty());

        // Menos filas de las que la task contó NO es «hecho».
        v.finish(7, 3);
        assert_eq!(
            v.state,
            CompareState::Incomplete,
            "3 filas recibidas contra 7 contadas: la respuesta está a medias y lo dice"
        );
    }
}
