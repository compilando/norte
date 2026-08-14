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

use norte_i18n::{Lang, t_in, ta_in};
use norte_proto::VPath;
use norte_proto::methods::{
    CompareConfidence, CompareCriterion, CompareReason, CompareRow, CompareVerdict, Side,
};

/// The mtime tolerance a frontend asks a comparison for: 2000 ms, the FAT
/// rule and the widest granularity that really exists.
///
/// It is a COPY of the wire's default (`FsCompareParams::mtime_tolerance_ms`),
/// because `norte-proto` keeps its default function private and making it
/// public would mean touching the protocol crate to read one number. That the
/// two never drift apart is pinned by `la_tolerancia_por_defecto_sigue_al_wire`,
/// in `norte-tui`: it deserialises a minimal params off the wire and compares.
/// It stays there and not here because this crate has no `serde_json`, not
/// even in dev-dependencies, and adding it to read one number would be the
/// same price that was refused by not touching `norte-proto`.
///
/// It lives next to the model, and not in each frontend, for the same reason
/// [`CompareView`] does (#158): it is a parameter of the QUESTION, so two
/// copies that drifted would have the TUI and the GUI receiving different
/// verdicts for the same two directories — and neither of them could see
/// it.
pub const MTIME_TOLERANCE_MS: u32 = 2000;

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
///     paired_under: None,
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

/// Por qué una fila enseña DOS ortografías, en una frase, o `None` cuando no
/// hay nada que explicar (#208, 0.42.0 `CompareRow::paired_under`).
///
/// Los dos paneles de diferencias pintan dos nombres que una fuente puede
/// rendir idénticos —el par NFC/NFD— o que son visiblemente caracteres
/// distintos —el KELVIN SIGN contra la `K` ASCII—, y hasta ahora no decían
/// nada de por qué están en la misma fila. Esto es lo que falta: una frase
/// fija y localizada, JAMÁS un badge pegado al nombre (misma regla que
/// `sync::dest_twin_label`, #192 — lo que se pega a un nombre lo puede
/// falsificar un nombre).
///
/// Tres respuestas y no cuatro, y la diferencia importa:
///
/// * `None` cuando no hubo transformación: la pareja es byte a byte.
/// * La frase SUAVE para [`PairTransform::CaseFold`](norte_proto::methods::PairTransform::CaseFold) y
///   [`PairTransform::Normalization`](norte_proto::methods::PairTransform::Normalization): son las parejas para las que la clave
///   existe, y refusarlas rompería el caso macOS↔Linux que sirve.
/// * La frase FUERTE para [`PairTransform::NormalizationSingleton`](norte_proto::methods::PairTransform::NormalizationSingleton) y para
///   cualquier transformación que este build no conozca — o sea, exactamente
///   cuando [`PairTransform::names_one_text`](norte_proto::methods::PairTransform::names_one_text) contesta `false`. Un singleton
///   puede estar juntando DOS FICHEROS DISTINTOS, y una transformación que un
///   daemon más nuevo nombró no se puede leer como inocua.
///
/// ```
/// use norte_frontend::compare::paired_under_label;
/// use norte_i18n::Lang;
/// use norte_proto::methods::PairTransform;
///
/// assert!(paired_under_label(None, Lang::En).is_none(), "sin transformación, sin frase");
/// let suave = paired_under_label(Some(PairTransform::Normalization), Lang::En)
///     .expect("una pareja NFC/NFD se explica");
/// let fuerte = paired_under_label(Some(PairTransform::NormalizationSingleton), Lang::En)
///     .expect("y un singleton, más fuerte");
/// assert_ne!(suave, fuerte, "la peligrosa no se dice igual que la corriente");
/// ```
#[must_use]
pub fn paired_under_label(
    paired_under: Option<norte_proto::methods::PairTransform>,
    lang: norte_i18n::Lang,
) -> Option<String> {
    let transform = paired_under?;
    Some(if transform.names_one_text() {
        t_in(lang, "compare-paired-under")
    } else {
        t_in(lang, "compare-paired-under-singleton")
    })
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
///     paired_under: None,
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
    /// La primera fila VISIBLE, que es PEGAJOSA (#210): se arrastra solo
    /// cuando el cursor se sale, igual que la del listado de ficheros. Antes
    /// se deducía del cursor en cada frame y eso lo dejaba clavado en la
    /// última fila — ver [`crate::viewport::sticky_offset`].
    viewport_offset: usize,
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
            viewport_offset: 0,
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
    ///
    /// Arithmetic on the cached `counts`, **not** a scan: every row falls in
    /// exactly one [`Category`] (`Category::of` is total), so the hidden
    /// buckets subtract exactly. This is O(5) and the scan was O(rows) — and
    /// a virtualised painter asks for it EVERY FRAME to size its list, on a
    /// collection that is deliberately unbounded and still growing while the
    /// walk feeds it. The GUI's diff pane is what made it matter; the TUI
    /// asks for it every frame too.
    #[must_use]
    pub fn visible_len(&self) -> usize {
        let hidden_rows: usize = self.hidden.iter().map(|c| self.count_of(*c)).sum();
        self.rows.len().saturating_sub(hidden_rows)
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

    /// Deja la ventana lista para pintar `rows` filas con el cursor donde
    /// está (#210): la arrastra SOLO si el cursor se salió. Se llama una vez
    /// por frame, antes de pintar.
    pub fn reconcile_viewport(&mut self, rows: usize) {
        self.viewport_offset = crate::viewport::sticky_offset(
            self.viewport_offset,
            self.visible_index().unwrap_or(0),
            self.visible_len(),
            rows,
        );
    }

    /// La primera fila visible — ver [`Self::reconcile_viewport`].
    #[must_use]
    pub fn viewport_offset(&self) -> usize {
        self.viewport_offset
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

/// The diff pane's status line: how the comparison is going (or how it
/// ended), which side the row commands act on, and how many rows are marked.
///
/// **Shared, and that is the whole point.** The five `compare-status-*`
/// variants are how a frontend says whether the answer is COMPLETE, and in a
/// comparison how complete the answer is *is* the answer. Two frontends
/// composing that sentence from their own copies is precisely how the CLI
/// (phase A) and the MCP tool (phase B) each ended up reporting a complete
/// answer for a run that had lost batches. One composition, both surfaces.
///
/// `marked` is data, not logic: the TUI prints its marked count here and the
/// GUI has no marking yet (its sync surface is #161), so it passes `0` and
/// gets the same sentence minus that clause.
///
/// ```
/// use norte_frontend::compare::{CompareState, CompareView, status_line};
/// use norte_i18n::Lang;
/// use norte_proto::VPath;
///
/// let mut v = CompareView::new(
///     VPath::parse("file:///a").expect("path"),
///     VPath::parse("file:///b").expect("path"),
///     0,
///     None,
///     None,
/// );
/// v.finish(7, 3);
/// assert_eq!(v.state, CompareState::Incomplete);
/// // Says BOTH counts: "done" over a half-answer is the bug this prevents.
/// let s = status_line(&v, 0, Lang::En);
/// assert!(s.contains('7') && s.contains('0'));
/// ```
#[must_use]
pub fn status_line(view: &CompareView, marked: usize, lang: Lang) -> String {
    let n = view.pane.len().to_string();
    let how = match view.state {
        CompareState::Running => ta_in(lang, "compare-status-running", &[("n", &n)]),
        CompareState::Done => ta_in(lang, "compare-status-done", &[("n", &n)]),
        CompareState::Unknown => ta_in(lang, "compare-status-unknown", &[("n", &n)]),
        CompareState::Incomplete => ta_in(
            lang,
            "compare-status-incomplete",
            &[("n", &n), ("total", &view.rows_expected.to_string())],
        ),
        CompareState::Cancelled => ta_in(lang, "compare-status-cancelled", &[("n", &n)]),
        // The error CATEGORY the view stored, painted PERSISTENTLY: a failure
        // must not decay into "done" because a frontend's transient banner
        // was cleared by the next keystroke.
        CompareState::Failed => ta_in(
            lang,
            "compare-status-failed",
            &[("error", view.error.as_deref().unwrap_or(""))],
        ),
    };
    let side = ta_in(
        lang,
        "compare-active-side",
        &[("side", &side_label(view.pane.active_side(), lang))],
    );
    // The marked count goes here and not on a key line: it is state, not
    // vocabulary. Only when there is one — a permanent "0 marked" would be
    // noise in the normal case.
    // #208: por qué la fila SELECCIONADA enseña dos ortografías, cuando las
    // enseña. Va aquí y no pegado al nombre por la misma razón que
    // `sync::dest_twin_label`: lo que se pega a un nombre lo puede falsificar
    // un nombre, y esta frase es justamente la que no debe poder falsificarse.
    // Aquí lo ven los DOS paneles —los dos pintan esta línea— con una sola
    // implementación y una sola traducción.
    let pareja = view
        .pane
        .selected_row()
        .and_then(|row| paired_under_label(row.paired_under, lang));
    let mut out = format!("{how} · {side}");
    if marked > 0 {
        let m = marked.to_string();
        out = format!("{out} · {}", ta_in(lang, "compare-marked", &[("n", &m)]));
    }
    if let Some(pareja) = pareja {
        out = format!("{out} · {pareja}");
    }
    out
}

/// How a directory comparison is going, as the pane presents it (`Shift+F2`,
/// 2026-08-11-directory-comparison.md): the run loop reflects it into
/// [`CompareView::state`] so the status bar can pick its `compare-status-*`
/// variant.
///
/// The same shape as a live search's `SearchState`, with ONE extra variant
/// and the reason it exists: in `fs.compare` the row channel closing does
/// **not** mean every row arrived. The rows pump and the terminal-snapshot
/// pump are independent tasks, so when the stream ends what was received is
/// compared against `TaskProgress::entries_done` — and if something is
/// missing, [`CompareState::Incomplete`] SAYS SO instead of painting "done"
/// over a half answer. In a comparison, how complete the answer is *is* the
/// answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CompareState {
    /// The walk is still emitting rows.
    #[default]
    Running,
    /// It ended, and every row the task counted arrived.
    Done,
    /// It ended, but FEWER rows arrived than the task counted: a batch was
    /// lost on the way.
    Incomplete,
    /// The run ended and **nothing said how**: the row channel closed without
    /// a terminal `TaskState` ever being observed, so there is no snapshot to
    /// compare the arrived rows against.
    ///
    /// This exists because the honest answer is neither of the two above
    /// (#183). There is a benign race in the design — the channel can close
    /// before the terminal state is seen — and both frontends used to paint
    /// `Done` for it, which is right for the race and WRONG for the case it
    /// cannot be told apart from: a task that died early, with no snapshot
    /// and no counts. `Done` is the one answer a comparison must never give
    /// when it does not know, because "how complete the answer is IS the
    /// answer" — the sentence [`CompareState::Incomplete`] is built on, and
    /// what the CLI's exit code 2 and the MCP tool's `complete: false` both
    /// rest on.
    Unknown,
    /// The user cancelled (the rows that did arrive are kept).
    Cancelled,
    /// The task failed (the error goes to the status bar).
    Failed,
}

/// The open diff pane (`Shift+F2`): the pure model, plus what a frontend
/// needs to paint it and to say how it ended.
///
/// The model (rows, filters, selection by id, active side) is deliberately
/// NOT here (hard rule 7): it lives in [`ComparePane`], where it is tested
/// without a terminal, and this struct only adds the run's state and the two
/// roots the header paints.
#[derive(Debug)]
pub struct CompareView {
    /// Rows, filters, selection and active side.
    pub pane: ComparePane,
    /// How the comparison is going (or how it ended).
    pub state: CompareState,
    /// The error CATEGORY of a comparison that FAILED, already localised and
    /// sanitised. It is painted PERSISTENTLY, like a live search's
    /// `search_error`: a failure must not decay into "done" on the next
    /// keystroke.
    pub error: Option<String>,
    /// How many rows the task counted (`TaskProgress::entries_done`) when the
    /// stream closed. Only meaningful with [`CompareState::Incomplete`],
    /// which is the only case where it differs from the rows that are here.
    pub rows_expected: u64,
    /// The left root: the pane that launched the comparison.
    pub left_root: VPath,
    /// The right root.
    pub right_root: VPath,
    /// Index of the pane that IS the left side — the one that launched the
    /// comparison, which need not be `panes[0]`.
    ///
    /// Frozen when the pane opens, and it decides WHICH pane a row's `Enter`
    /// navigates: the one belonging to the ACTIVE side. Without it `Enter`
    /// always went to the focused pane, so a reader looking at the right side
    /// lost their left directory to go and see the right one — caught by the
    /// tmux harness, and no model assertion could have seen it.
    pub left_pane: usize,
    /// Cancellation has already been requested for this comparison (the first
    /// `Esc`).
    ///
    /// The second `Esc` closes the pane WHATEVER happens to the Task. Without
    /// this, closing depended on the row channel actually closing, and there
    /// are ways for it not to — a dead daemon, a provider hung on a dead NFS
    /// — which left the reader trapped in the one norte screen with no exit
    /// (review BLOCKER-1).
    pub cancel_requested: bool,
    /// Name reinterpretation (#57) for EACH side, frozen when the pane opens.
    ///
    /// Two and not one: the two panes are two locations and may carry
    /// different overrides. Without this, a reader who had pressed `Alt+E` to
    /// read a CP1251 share got `????.txt` back the moment they compared it
    /// (review MAJOR-3).
    pub left_encoding: Option<norte_encoding::NameEncoding>,
    /// The right side's.
    pub right_encoding: Option<norte_encoding::NameEncoding>,
}

impl CompareView {
    /// A freshly opened pane over these two roots, with no rows yet.
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

    /// The row channel closed: decides the terminal state from what the task
    /// COUNTED (`entries_done`, off `TaskProgress`) against what actually
    /// ARRIVED (`rows_received`).
    ///
    /// This, and not the channel closing, is what says whether the comparison
    /// really finished: the rows pump and the terminal-snapshot pump are
    /// independent tasks (see [`CompareState::Incomplete`]), so a closed
    /// channel with fewer rows than were counted is a lost batch, not a
    /// complete answer. The CLI (phase A) and the MCP tool (phase B)
    /// reimplemented this count on their own and both got it wrong in exactly
    /// this spot — hence it lives with the model and not in each frontend.
    ///
    /// It decides neither `Cancelled` nor `Failed`: those come straight off
    /// the run's `TaskState` and never off a row count. Prefer
    /// [`CompareView::finish_from_task`], which owns that whole mapping.
    pub fn finish(&mut self, entries_done: u64, rows_received: u64) {
        self.rows_expected = entries_done;
        self.state = if rows_received < entries_done {
            CompareState::Incomplete
        } else {
            CompareState::Done
        };
    }

    /// The row channel closed: maps the `TaskState` the run carried at that
    /// instant onto the pane's state, and returns the typed error if there
    /// was one — so the caller can ALSO put up its transient notice (a status
    /// bar, a banner), which is the only part that differs between frontends.
    ///
    /// **The four arms, and one copy of them.** They were transcribed by hand
    /// into `norte-tui` and into `norte-gui`, with a FOURTH copy of the
    /// arithmetic in the TUI's already-closed-pane branch. Only the
    /// `Completed` arm had reached [`CompareView::finish`], which is half the
    /// job: the day the rule changes — #183 — the fix has to land in one
    /// place, or the two frontends will disagree about whether a FAILED
    /// comparison is complete, which is the bug the CLI (phase A) and the MCP
    /// tool (phase B) each shipped on their own.
    ///
    /// * `Cancelled` and `Failed` come off the `TaskState`, **never** off
    ///   counting rows: a cancelled comparison lost nothing, it simply did
    ///   not continue, and its rows are still true.
    /// * `Completed` is the ONLY arm that goes through
    ///   [`CompareView::finish`]'s count, which is the only situation where
    ///   missing rows mean rows were LOST.
    /// * a **non-terminal** state is the benign race: the channel closed
    ///   before the terminal snapshot was published (the two pumps are
    ///   independent tasks), so `entries_done` is not final yet and `Done` is
    ///   painted with what is here. Accusing that race of losing rows is the
    ///   same mistake in reverse.
    ///
    /// The failure's category is stored already localised in the language
    /// ASKED FOR (see
    /// [`error_category_in`](crate::error::error_category_in)): the same one
    /// [`status_line`] composes with, which is what paints it.
    ///
    /// ```
    /// use norte_frontend::compare::{CompareState, CompareView};
    /// use norte_i18n::Lang;
    /// use norte_proto::{TaskState, VPath};
    ///
    /// let mut v = CompareView::new(
    ///     VPath::parse("file:///a").expect("path"),
    ///     VPath::parse("file:///b").expect("path"),
    ///     0,
    ///     None,
    ///     None,
    /// );
    /// // Cancelling with 2 of 9 rows is NOT a loss.
    /// assert!(v.finish_from_task(&TaskState::Cancelled, 9, 2, Lang::En).is_none());
    /// assert_eq!(v.state, CompareState::Cancelled);
    /// ```
    pub fn finish_from_task<'a>(
        &mut self,
        state: &'a norte_proto::TaskState,
        entries_done: u64,
        rows_received: u64,
        lang: Lang,
    ) -> Option<&'a norte_proto::Error> {
        use norte_proto::TaskState;
        match state {
            TaskState::Completed => {
                self.finish(entries_done, rows_received);
                None
            }
            TaskState::Cancelled => {
                self.state = CompareState::Cancelled;
                self.rows_expected = entries_done;
                None
            }
            TaskState::Failed { error } => {
                self.state = CompareState::Failed;
                self.rows_expected = entries_done;
                // The localised CATEGORY, never the English `Display`: this
                // is painted PERSISTENTLY in the footer, and several error
                // variants interpolate data from the peer.
                self.error = Some(crate::error::error_category_in(lang, error));
                Some(error)
            }
            // NO terminal: el canal de filas se cerró sin que nadie observara
            // el desenlace. Esto era `Done` y ahí estaba el agujero (#183) —
            // decía «terminó y llegó todo» sobre una task que pudo morir sin
            // publicar nada.
            _ => {
                self.state = CompareState::Unknown;
                self.rows_expected = entries_done;
                None
            }
        }
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
    use norte_proto::{Entry, EntryKind, TaskState, VPath};

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
            paired_under: None,
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

    /// **Branch review, MAJOR-1.** The `TaskState` → pane-state mapping lives
    /// HERE, once. It was transcribed by hand into both frontends (plus a
    /// FOURTH copy of the arithmetic in the TUI's already-closed-pane
    /// branch), which is exactly what task 1 existed to prevent: when #183 is
    /// fixed, the fix has to land in one place, not three.
    #[test]
    fn the_task_state_decides_the_verdict_in_one_place() {
        let view = || {
            CompareView::new(
                VPath::parse("file:///a").expect("wire"),
                VPath::parse("file:///b").expect("wire"),
                0,
                None,
                None,
            )
        };

        // `Completed` is the ONLY arm that goes through the count.
        let mut v = view();
        assert!(
            v.finish_from_task(&TaskState::Completed, 9, 7, Lang::En)
                .is_none()
        );
        assert_eq!(v.state, CompareState::Incomplete, "9 counted, 7 arrived");
        assert_eq!(v.rows_expected, 9);

        let mut v = view();
        v.finish_from_task(&TaskState::Completed, 7, 7, Lang::En);
        assert_eq!(v.state, CompareState::Done);

        // Cancelling is NOT losing: it comes off the `TaskState`, never off
        // counting rows.
        let mut v = view();
        v.finish_from_task(&TaskState::Cancelled, 9, 2, Lang::En);
        assert_eq!(v.state, CompareState::Cancelled);
        assert_eq!(v.rows_expected, 9);
        assert!(v.error.is_none());

        // Nor is failing, and the CATEGORY is stored already localised — the
        // typed error comes back so the caller can add its own banner.
        let mut v = view();
        let failure = TaskState::Failed {
            error: norte_proto::Error::PermissionDenied,
        };
        let returned = v.finish_from_task(&failure, 9, 2, Lang::Es);
        assert!(matches!(
            returned,
            Some(norte_proto::Error::PermissionDenied)
        ));
        assert_eq!(v.state, CompareState::Failed);
        assert_eq!(
            v.error.as_deref(),
            Some(
                crate::error::error_category_in(Lang::Es, &norte_proto::Error::PermissionDenied)
                    .as_str()
            ),
            "in the language ASKED FOR, not the ambient one"
        );

        // The channel closed before a terminal state was published. This was
        // `Done` until #183, and the reasoning for that was half right: the
        // benign race — the two pumps are independent, so the channel CAN
        // close first — must not be accused of loss, because reporting
        // `Incomplete` for a run that was fine is the CLI's and the MCP
        // tool's mistake in reverse.
        //
        // What it missed is that the same arm also covers a task that DIED
        // before publishing anything, and at this instant the two are
        // indistinguishable. `Done` is the one answer a comparison must never
        // give when it does not know. So neither: a third state that says
        // exactly what happened.
        for non_terminal in [
            TaskState::Pending,
            TaskState::Running,
            TaskState::Paused,
            TaskState::Unknown,
        ] {
            let mut v = view();
            v.finish_from_task(&non_terminal, 9, 2, Lang::En);
            assert_eq!(
                v.state,
                CompareState::Unknown,
                "{non_terminal:?}: nothing said how the run ended, so neither does the pane"
            );
        }

        // And a terminal state with the counts agreeing is STILL `Done`: the
        // fix above must not turn every healthy comparison into a shrug.
        let mut v = view();
        v.finish_from_task(&TaskState::Completed, 9, 9, Lang::En);
        assert_eq!(v.state, CompareState::Done);
    }

    /// **Branch review, MAJOR-2.** `visible_len` stopped walking and became
    /// arithmetic over the cached counts, but `move_by` still resolves with
    /// `visible().nth()` and the GUI's virtualised list is SIZED by the count
    /// while it is FILLED by the walk. If the two ever disagree the failure is
    /// SILENT: the rows past the miscount become unreachable and `End` stops
    /// moving — a short list in a pane whose whole subject is whether the
    /// answer is complete.
    ///
    /// All 32 subsets of the five categories, over rows from every one.
    #[test]
    fn the_visible_count_matches_the_visible_walk() {
        let rows: Vec<CompareRow> = [
            Same,
            Same,
            Different,
            OnlyLeft,
            CompareVerdict::OnlyRight,
            Error,
            CompareVerdict::TypeMismatch,
            CompareVerdict::Ambiguous,
        ]
        .into_iter()
        .enumerate()
        .map(|(i, v)| row_id(i as u64 + 1, v))
        .collect();

        for mask in 0u32..(1 << CATEGORIES.len()) {
            let mut pane = pane_with(rows.clone());
            for (i, category) in CATEGORIES.into_iter().enumerate() {
                if mask & (1 << i) != 0 {
                    pane.toggle_filter(category);
                }
            }
            let walked = pane.visible().count();
            assert_eq!(
                pane.visible_len(),
                walked,
                "mask {mask:#07b}: the cached count drifted from the walk"
            );
            // And what the arithmetic says is there is REACHABLE: the list's
            // `End` resolves by `nth`, not by the count.
            pane.select_last();
            if walked > 0 {
                assert_eq!(
                    pane.visible_index(),
                    Some(walked - 1),
                    "mask {mask:#07b}: the last visible row is unreachable"
                );
            }
        }
    }

    /// #208: la fila SELECCIONADA explica por qué enseña dos ortografías, y la
    /// peligrosa no se dice igual que la corriente. Va en la línea de estado
    /// —que pintan los DOS paneles, con una sola traducción— y jamás pegada al
    /// nombre: lo que se pega a un nombre lo puede falsificar un nombre.
    #[test]
    fn la_linea_de_estado_explica_la_pareja_seleccionada() {
        use norte_proto::methods::PairTransform;

        let armar = |paired: Option<PairTransform>| {
            let mut v = CompareView::new(
                VPath::parse("file:///a").expect("wire"),
                VPath::parse("file:///b").expect("wire"),
                0,
                None,
                None,
            );
            let mut fila = row_id(1, Different);
            fila.paired_under = paired;
            v.pane.extend(vec![fila]);
            status_line(&v, 0, Lang::En)
        };

        let singleton = paired_under_label(Some(PairTransform::NormalizationSingleton), Lang::En)
            .expect("la peligrosa tiene frase");
        let corriente =
            paired_under_label(Some(PairTransform::Normalization), Lang::En).expect("y la NFC/NFD");
        assert_ne!(singleton, corriente, "no se dicen igual");

        assert!(
            armar(Some(PairTransform::NormalizationSingleton)).contains(&singleton),
            "un singleton se avisa"
        );
        assert!(
            armar(Some(PairTransform::Normalization)).contains(&corriente),
            "y una pareja NFC/NFD se explica"
        );
        // Byte a byte: no hay nada que explicar y no se dice nada.
        let limpia = armar(None);
        assert!(
            !limpia.contains(&corriente) && !limpia.contains(&singleton),
            "{limpia}"
        );
    }

    /// **Branch review, MAJOR-3.** `status_line` moved here with five states ×
    /// two `marked` arms, and the only witness that came with it was a TUI
    /// snapshot in `Done` with nothing marked: one of the ten branches. The
    /// other nine crossed crates with nothing holding them.
    #[test]
    fn the_footer_says_every_state_and_the_marks_in_both_locales() {
        // La lista es EXHAUSTIVA a mano, así que una variante nueva que
        // nadie añada aquí pasa sin que su cadena exista en los dos idiomas —
        // que es el síntoma que este test caza. `Unknown` entró con #183.
        let states = [
            CompareState::Running,
            CompareState::Done,
            CompareState::Incomplete,
            CompareState::Unknown,
            CompareState::Cancelled,
            CompareState::Failed,
        ];
        for lang in [Lang::En, Lang::Es] {
            for state in states {
                for marked in [0usize, 3] {
                    let mut v = CompareView::new(
                        VPath::parse("file:///a").expect("wire"),
                        VPath::parse("file:///b").expect("wire"),
                        0,
                        None,
                        None,
                    );
                    v.pane.extend(vec![row_id(1, Same), row_id(2, OnlyLeft)]);
                    v.state = state;
                    v.rows_expected = 7;
                    v.error = Some(crate::error::error_category_in(
                        lang,
                        &norte_proto::Error::PermissionDenied,
                    ));
                    let s = status_line(&v, marked, lang);

                    // No branch leaves a Fluent id unresolved: a raw
                    // `compare-status-*` in the footer is exactly the symptom
                    // of a missing translation.
                    assert!(
                        !s.contains("compare-status-") && !s.contains("compare-active-side"),
                        "{state:?}/{marked}/{lang:?}: unresolved id in «{s}»"
                    );
                    // The active side is ALWAYS there, whatever the run did.
                    assert!(
                        s.contains(&side_label(v.pane.active_side(), lang)),
                        "{state:?}/{marked}/{lang:?}: no active side in «{s}»"
                    );
                    // The counts: the rows that are here and, in `Incomplete`,
                    // ALSO the ones that were counted — saying "done" over
                    // half an answer is the bug this footer exists to avoid.
                    if state == CompareState::Failed {
                        assert!(
                            s.contains(&crate::error::error_category_in(
                                lang,
                                &norte_proto::Error::PermissionDenied
                            )),
                            "{lang:?}: the failure does not say its category in «{s}»"
                        );
                    } else {
                        assert!(s.contains('2'), "{state:?}/{lang:?}: no row count in «{s}»");
                    }
                    if state == CompareState::Incomplete {
                        assert!(s.contains('7'), "{lang:?}: no total in «{s}»");
                    }
                    // And the marks clause is there if and only if there are
                    // marks.
                    assert_eq!(
                        s.contains('3'),
                        marked > 0,
                        "{state:?}/{marked}/{lang:?}: the marks clause does not add up in «{s}»"
                    );
                }
            }
        }
    }

    /// #158: the run's state used to live in `norte-tui`, so the GUI would
    /// have had to reimplement it — and the two surfaces that already did
    /// (the CLI in phase A, the MCP tool in phase B) got the same thing
    /// wrong: they called an answer complete when batches were missing. Here,
    /// and once.
    #[test]
    fn the_runs_state_lives_with_the_model() {
        let mut v = CompareView::new(
            VPath::parse("file:///a").expect("wire"),
            VPath::parse("file:///b").expect("wire"),
            0,
            None,
            None,
        );
        assert_eq!(v.state, CompareState::Running, "it is born running");
        assert!(v.pane.is_empty());

        // Fewer rows than the task counted is NOT "done".
        v.finish(7, 3);
        assert_eq!(
            v.state,
            CompareState::Incomplete,
            "3 rows received against 7 counted: the answer is partial and says so"
        );
    }
}
