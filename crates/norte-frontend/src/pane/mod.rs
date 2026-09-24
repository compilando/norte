//! PURE state (no render) of a pane for norte's frontends. The GUI (GPUI)
//! consumes [`PaneState`] as its model of a navigable panel: cursor, quick
//! search, and the current listing, with not a single UI dependency.
//!
//! The TUI's `Pane` (`norte-tui::app`) embeds this `PaneState` and delegates
//! to it all the pure cursor + quick search mechanics (#82 closed): the
//! duplication was removed; the TUI only adds its render state, ratatui
//! scroll, and live search ON TOP of `PaneState`.
//!
//! Includes the refresh-after-batch contract for the paginated fill (ADR
//! 0017): [`PaneState::extend`] (adds a batch, re-sorts, and re-anchors the
//! cursor by path), [`PaneState::refill`] (replaces the same dir's listing
//! with a clamp), and [`PaneState::refresh_quick`] (re-applies the live
//! filter). The GUI today lists all at once and does not use them; the TUI
//! does.

mod marks;
mod quick;
mod viewport;

pub use marks::MarksSummary;

// `PaneState` is still ONE: what is split up are its methods, in sibling
// `impl` blocks. A child module sees its parent's private items, so this
// opens nothing — it only groups together what is read together.

use crate::decoration::Decoration;
use crate::nav::{Mode, QuickSearch};
use crate::sort::SortKey;
use globset::GlobBuilder;
use norte_proto::{Entry, EntryKind, VPath};
use std::collections::{HashMap, HashSet};

/// Why a mark-by-pattern was rejected (hard rule 6: typed library errors).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PatternError {
    /// The glob does not compile. Carries the `globset` diagnostic, which
    /// EMBEDS the user's pattern verbatim — a frontend MUST mask it before
    /// painting it (`display_name`), exactly as it masks a file name: a
    /// pattern arrives by paste as easily as by typing, and can carry bidi
    /// overrides or invisibles. It quotes the FOLDED pattern
    /// ([`PaneState::mark_glob`] lowercases and NFC-normalises via
    /// `nav::fold` before compiling), not what the user typed — `ABC[`
    /// reports `'abc['`, text the user never typed.
    #[error("{0}")]
    Glob(String),
}

/// Recompiles a [`globset::Glob`]'s byte-mode regex in Unicode mode (#110):
/// globset compiles with `(?-u)`, where `?` consumes ONE BYTE and a class
/// matches byte for byte — `a?o` did not match `año` (`ñ` = 2 bytes) and
/// `a[ñx]o` matched `axo` but NEVER `año`, silently marking a different file
/// than the one named. globset remains the ONLY syntax authority (same lib
/// as `fs.search`): this only translates its output.
///
/// The translation decodes runs of `\xNN` escapes with NN ≥ 0x80 —the ONLY
/// way globset emits the pattern's non-ASCII bytes (`&str`, so the runs are
/// ALWAYS complete UTF-8)— back into their characters, which are never regex
/// metacharacters and go literal both inside and outside a class. A class
/// range with multibyte ends (`[ñ-ü]` → `[\xc3\xb1-\xc3\xbc]`) also falls out
/// right: the ASCII `-` cuts the run and each end decodes to its char.
/// `(?-u)` is stripped from the prefix; the rest of the flags pass through
/// as-is.
///
/// Deliberate COPY of `norte-core::search`'s translator (same criterion as
/// the fold, duplicated core/frontend): there is no common crate under both
/// where it would fit without dragging `globset`+`regex` into an unrelated
/// crate. Each copy pins globset's shape with its own guard test.
///
/// # Errors
/// [`PatternError::Glob`] if a decoded run is not valid UTF-8 — should not
/// happen with the pinned globset (guard test
/// `globset_regex_shape_is_the_one_this_translation_expects`); fail loud
/// rather than match bytes the user never wrote.
fn unicode_glob_regex(glob: &globset::Glob) -> Result<String, PatternError> {
    let src = glob.regex();
    let stripped = src.strip_prefix("(?-u)").unwrap_or(src);
    let mut out = String::with_capacity(stripped.len());
    let mut run: Vec<u8> = Vec::new();
    let flush = |run: &mut Vec<u8>, out: &mut String| -> Result<(), PatternError> {
        if run.is_empty() {
            return Ok(());
        }
        let decoded = std::str::from_utf8(run).map_err(|_| {
            PatternError::Glob(
                "internal: the glob compiled to byte escapes that do not \
                 form UTF-8 characters"
                    .to_owned(),
            )
        })?;
        out.push_str(decoded);
        run.clear();
        Ok(())
    };
    let bytes = stripped.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // `stripped.get(..)` and not a direct slice: if a `\x` preceded a
        // multibyte char, the i+2..i+4 range would split the char and a
        // direct slice would PANIC — unreachable with the pinned globset,
        // but this function fails through a Result, not a panic.
        if bytes[i] == b'\\'
            && bytes.get(i + 1) == Some(&b'x')
            && let Some(hex) = stripped.get(i + 2..i + 4)
            && let Ok(b) = u8::from_str_radix(hex, 16)
            && b >= 0x80
        {
            run.push(b);
            i += 4;
            continue;
        }
        flush(&mut run, &mut out)?;
        // Copies the rest as-is — including ASCII escapes (`\.`), whose
        // meaning is identical in Unicode mode.
        let step = if bytes[i] == b'\\' && i + 1 < bytes.len() {
            1 + stripped[i + 1..].chars().next().map_or(0, char::len_utf8)
        } else {
            stripped[i..].chars().next().map_or(1, char::len_utf8)
        };
        out.push_str(&stripped[i..i + step]);
        i += step;
    }
    flush(&mut run, &mut out)?;
    Ok(out)
}

/// Hidden entry? (#107): decided by the BYTES of the LAST segment — rule 1
/// demands it — with the unix criterion of a leading `.` (0x2E). `a.txt` is
/// not one; a non-UTF8 name starting with 0x2E is. Windows's hidden
/// attribute will arrive through the wire's attrs (proto 0.30) once some
/// provider announces it.
fn is_hidden_entry(e: &Entry) -> bool {
    e.path
        .file_name()
        .is_some_and(|n| n.as_bytes().first() == Some(&b'.'))
}

/// Buckets of the name-width histogram: the last one counts everything that
/// measures 64 cells or more, which is already more than any panel will
/// give it.
const NAME_WIDTH_BUCKETS: usize = 65;

/// Adds each of `entries`' name width (cells) to `buckets`.
///
/// Nothing is allocated per entry: a UTF-8 name is measured in place, and
/// only one that is not goes through the lossy conversion — the same one
/// that paints it. The per-pane encoding reinterpretation is not looked at:
/// it changes which glyphs come out, not how many fit.
fn measure_names(buckets: &mut [u32; NAME_WIDTH_BUCKETS], entries: &[Entry]) {
    use unicode_width::UnicodeWidthStr;
    for e in entries {
        let bytes = e.path.file_name().map_or(&[][..], |n| n.as_bytes());
        let width = match std::str::from_utf8(bytes) {
            Ok(s) => s.width(),
            Err(_) => String::from_utf8_lossy(bytes).width(),
        };
        let bucket = width.min(NAME_WIDTH_BUCKETS - 1);
        buckets[bucket] = buckets[bucket].saturating_add(1);
    }
}

/// `entries`' name-width histogram, from scratch.
fn buckets_of(entries: &[Entry]) -> [u32; NAME_WIDTH_BUCKETS] {
    let mut buckets = [0u32; NAME_WIDTH_BUCKETS];
    measure_names(&mut buckets, entries);
    buckets
}

/// What state a listing's `..` row is in.
///
/// An enum and not two `bool`s because the fourth state two booleans would
/// allow —"not requested but present"— does not exist, and because the
/// difference between the two that DO exist is exactly the one that gets
/// forgotten: at a ROOT it is requested and not present, and confusing them
/// would make the listing's first real entry behave like the go-up row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParentRow {
    /// The configuration does not want it.
    Off,
    /// It wants it, but there is none here: this directory has no parent.
    Requested,
    /// It is at `entries[0]`.
    Present,
}

/// Non-render state of a pane: directory, entries (normalised internally —
/// no longer requires a pre-sorted caller, see [`PaneState::new`]), cursor
/// and quick search.
///
/// The cursor is an index into `entries` (0 even with an empty list). With a
/// quick search active in [`Mode::Filter`] mode, the SELECTION lives inside
/// the quick search (the real cursor does not move until confirmed); in
/// [`Mode::Jump`] the real cursor jumps straight to the match.
#[derive(Debug)]
pub struct PaneState {
    dir: VPath,
    entries: Vec<Entry>,
    /// What state the `..` row is in (`[ui] parent_entry`).
    parent_row: ParentRow,
    /// Persisted sort keys, index-parallel to `entries` (#54): the fill
    /// merges batches O(n+m) without recomputing the NFC key of what was
    /// already listed.
    sort_keys: Vec<SortKey>,
    cursor: usize,
    loading: bool,
    quick: Option<QuickSearch>,
    marks: HashSet<VPath>,
    /// The selection from BEFORE the last bulk gesture, for `mark.restore`
    /// (#313). ONE snapshot per panel, neither a history nor something
    /// persisted: it is the net for whoever presses "unmark all" by
    /// accident, and with two snapshots nobody would know which one to go
    /// back to.
    ///
    /// `None` = there has been no bulk gesture since this panel exists (or
    /// since the last `cd`, which drops it: another directory's paths name
    /// nothing in this listing).
    marks_previous: Option<HashSet<VPath>>,
    /// Snapshot of `marks` from before the pointer sweep in progress, so
    /// that [`Self::apply_sweep`] can RESTORE it and re-mark, making a drag
    /// that retreats give back the rows it pulled off. `None` = no sweep, or
    /// a sweep armed but not yet applied (the clone is deliberately lazy:
    /// a plain click arms a sweep it usually never uses, and cloning the
    /// mark set on every click would be a cost nobody asked for).
    ///
    /// Dropped by every listing change ([`Self::set_listing`],
    /// [`Self::begin_loading`], [`Self::refill`]): a baseline is a claim
    /// about entries that were listed, and restoring it over a listing that
    /// moved underneath would resurrect marks the prune already dropped.
    sweep_baseline: Option<HashSet<VPath>>,
    /// Bumped by every change that can MOVE an index into
    /// [`Self::entries`]: a new listing, a cd, a refill, a page of an
    /// incremental fill, a re-sort, a hidden-entries toggle. Read through
    /// [`Self::listing_epoch`].
    ///
    /// It exists because an index is the only thing a pointer gesture
    /// carries. A frontend resolves a click against the frame it painted
    /// and then feeds that index back — to a sweep, to a double click —
    /// and between the two the listing can move underneath. This is the
    /// one signal that says so, so that a frontend can drop a gesture that
    /// has stopped meaning anything instead of applying it to whatever now
    /// occupies that row.
    ///
    /// Deliberately NOT the same thing as dropping `sweep_baseline`: that
    /// one is a claim about mark IDENTITY (paths), and it is kept where it
    /// already was.
    listing_epoch: u64,
    /// The listing's name-width histogram, which [`Self::name_width_p80`]
    /// comes from. Measured when the listing changes, not while painting: it
    /// is what [`crate::columns::fitted_columns`] tries to give the name,
    /// and measuring it per frame would mean walking the whole directory on
    /// every keystroke. A paginated fill's batch ADDS its names instead of
    /// re-measuring: `extend` is O(n+m) on purpose, and re-measuring would
    /// make it quadratic in a large directory.
    name_buckets: [u32; NAME_WIDTH_BUCKETS],
    /// Extent (`lo..=hi`, clamped nowhere) the sweep in progress applied
    /// last, so the next [`Self::apply_sweep`] can give back exactly the
    /// rows that left the range instead of rebuilding the mark set.
    sweep_extent: Option<(usize, usize)>,
    /// Marks dropped by the last [`Self::refill`] because their entry was gone
    /// (#103). The frontends surface it: a selection that shrinks behind the
    /// user's back must never be silent, because [`Self::marked_paths`] falls
    /// back to the CURSOR entry once the set empties — a silent prune would
    /// retarget the next bulk operation onto something nobody marked.
    pruned_marks: usize,
    /// Reinterpretation of non-UTF8 NAMES for display (#57, spec §6.1):
    /// `Some(enc)` = "see names as enc" — display ONLY, the bytes are never
    /// mutated (rule 1). Shared by the frontends (#98/m2): the quick search
    /// folds over the reinterpreted text and the renders read it through
    /// [`PaneState::name_encoding`]. Persistent per pane.
    name_encoding: Option<norte_encoding::NameEncoding>,
    /// Index of the cycle position the active reinterpretation ENTERED at
    /// (chardetng's suggestion, or 0): the cycle goes the FULL WAY AROUND and
    /// turns off on returning here — without this, encodings before the
    /// suggestion would be unreachable (M1 of review #57).
    name_encoding_entry: usize,
    /// Skipped from the current listing's CONTAINER (#93/#96): entries the
    /// archive provider's index discarded (hostile names/limits) and that
    /// are therefore NOT in `entries` — an incomplete listing is never
    /// silent. `None` = not applicable/unknown; the frontends only paint
    /// `Some(n)` with `n > 0`. Reset on every new listing
    /// ([`Self::set_listing`]); the caller sets it with the FRESH value from
    /// its `list_with_skipped`/`list_stream`.
    skipped: Option<u64>,
    /// Per-directory cursor memory (spec 2026-07-24 §S1): session-only,
    /// per-pane (does not persist across restarts — same precedent as
    /// [`crate::nav`]'s history), recency-LRU with a cap of
    /// [`CURSOR_MEMORY_CAP`]. Directory identity by BYTE-EXACT [`VPath`]
    /// (rule 1): never normalised, so two hostile twins with the same visual
    /// shape but different bytes are DIFFERENT entries. Fed by
    /// [`Self::remember_cursor`] and read from [`Self::set_listing`].
    cursor_memory: Vec<(VPath, usize)>,
    /// Pending focus from a `nav.parent` (spec §S1): the child we came from,
    /// to select it in the parent's listing. Wins over
    /// [`Self::cursor_memory`] and is CONSUMED (once) on the next
    /// [`Self::set_listing`], whether or not it matches an entry of the
    /// listing. Identity by byte-exact `VPath`, same as the memory.
    pending_focus: Option<VPath>,
    /// The listing's chosen sort order (#108 L7). Default = name/asc/dirs-
    /// first (the historical order). Changed by [`PaneState::set_sort`],
    /// which re-sorts in place, re-anchoring the cursor by path.
    sort: crate::sort::SortSpec,
    /// Show hidden entries (#107). `true` by default (the constructor knows
    /// nothing about config; the frontend sets `[ui] show_hidden`'s default
    /// with [`Self::set_show_hidden`] after building). PRESENTATION ONLY
    /// (rule 7 in reverse: the decision lives here, shared, and the provider
    /// still lists everything).
    show_hidden: bool,
    /// Entries set aside by hiding (#107): the ones whose last segment
    /// starts with `.` when `show_hidden == false`. They are returned to the
    /// listing (sorted merge) when shown again — set aside, not dropped, so
    /// the toggle needs no re-listing. Empty with `show_hidden` on.
    hidden_stash: Vec<Entry>,
    /// Per-entry plugin decorations (G3b, ADR 0037), ALREADY sanitised
    /// ([`crate::decoration::sanitize_decoration`]): the entry's badge/role,
    /// if some consented decorator decorated this path. Filled in
    /// ASYNCHRONOUSLY after the listing (never blocks `set_listing`, see the
    /// caller in each frontend) and that is why it lives OUTSIDE the normal
    /// `set_listing`/`begin_loading` reset — [`Self::set_listing`] and
    /// [`Self::begin_loading`] DO clear it (a new listing invalidates the
    /// previous one's decorations; they arrive late, not silently wrong
    /// until then) through [`Self::clear_decorations`].
    decorations: HashMap<VPath, Decoration>,
    /// Per-entry `plugin:` column values (#117-follow-up), an async mirror of
    /// `decorations`: outer key = the column's Display id
    /// (`plugin:<p>/<c>`), inner = the CURRENT listing's `VPath` → value
    /// ALREADY sanitised at ingest ([`crate::columns::sanitize_cell`]).
    /// [`Self::set_listing`]/[`Self::begin_loading`] clear it (a new listing
    /// invalidates the previous one's values).
    plugin_columns: HashMap<String, HashMap<VPath, String>>,
    /// Listing rows the frontend painted for this pane on the LAST frame
    /// (#124). The real height is decided by the widget when painting, so
    /// the model cannot deduce it: the frontend REPORTS it back with
    /// [`Self::set_viewport_rows`], and from that come the page jump
    /// ([`Self::page_step`]) and the stat probe's radius
    /// ([`Self::needs_stat_window`]) — before, these were constants that lied
    /// in any terminal that did not measure exactly that. `None` = not
    /// painted yet (or the pane is covered, e.g. with the viewer open):
    /// the caller's fallback rules.
    viewport_rows: Option<usize>,
    /// The listing's first VISIBLE row: the viewport, which is STICKY.
    ///
    /// It used to be deduced from the cursor on every frame (`selected -
    /// (height-1)`), and that anchors the cursor to the LAST row: past the
    /// first screen, every keystroke moved the content instead of the
    /// cursor, and moving back up scrolled the list down with it without the
    /// cursor ever peeling off the edge. An orthodox file manager does the
    /// opposite — the cursor moves INSIDE the viewport and only drags it when
    /// it touches an edge— and for that the viewport has to remember where
    /// it was.
    ///
    /// Reconciled once per frame ([`Self::reconcile_viewport`]), BEFORE
    /// painting: both the painting and the mouse hit test read this same
    /// number, which is what stops a click from landing on a different row.
    viewport_offset: usize,
}

/// Page jump with no frame painted yet (#124): the historical value, only
/// until the first [`PaneState::set_viewport_rows`].
pub const DEFAULT_PAGE: usize = 10;

/// Cap of the per-pane cursor memory (spec §S1): a long session with no
/// memory leak without depending on a new dependency (a hand-rolled LRU over
/// a `Vec`, cheap for dozens of visited dirs).
const CURSOR_MEMORY_CAP: usize = 64;

impl PaneState {
    /// A pane over `dir` with `entries` (normalised internally: no longer
    /// requires sorting them beforehand — the "sort them first" contract
    /// stops being a footgun, see [`sort_entries`](crate::sort_entries) for
    /// the criterion). Cursor at 0, no quick search, no pending load.
    #[must_use]
    pub fn new(dir: VPath, entries: Vec<Entry>) -> Self {
        let (entries, sort_keys) =
            crate::sort::sort_with_keys_spec(entries, &crate::sort::SortSpec::default());
        let name_buckets = buckets_of(&entries);
        Self {
            name_buckets,
            dir,
            entries,
            sort_keys,
            cursor: 0,
            loading: false,
            quick: None,
            marks: HashSet::new(),
            marks_previous: None,
            sweep_baseline: None,
            sweep_extent: None,
            listing_epoch: 0,
            pruned_marks: 0,
            name_encoding: None,
            name_encoding_entry: 0,
            skipped: None,
            cursor_memory: Vec::new(),
            pending_focus: None,
            show_hidden: true,
            hidden_stash: Vec::new(),
            sort: crate::sort::SortSpec::default(),
            decorations: HashMap::new(),
            plugin_columns: HashMap::new(),
            viewport_rows: None,
            viewport_offset: 0,
            parent_row: ParentRow::Off,
        }
    }

    /// Turns this listing's `..` row on or off (`[ui] parent_entry`).
    ///
    /// Requested once, when the pane is mounted, and kept across listings:
    /// it is configuration, not navigation state.
    pub fn set_parent_row(&mut self, on: bool) {
        if on != matches!(self.parent_row, ParentRow::Off) {
            return;
        }
        self.remove_parent_row();
        self.parent_row = if on {
            ParentRow::Requested
        } else {
            ParentRow::Off
        };
        self.insert_parent_row();
    }

    /// Is row `i` the `..` one?
    ///
    /// Both renderers ask it —to paint `..` instead of the parent
    /// directory's name— and so does navigation. Nobody else should need it:
    /// what stops that row from being an operation's OPERAND is that
    /// [`Self::selected`] returns `None` over it, not that every call site
    /// remembers to ask.
    #[must_use]
    pub fn is_parent_row(&self, i: usize) -> bool {
        self.has_parent_row() && i == 0
    }

    /// Where the `..` row leads, if there is one: the parent directory.
    #[must_use]
    pub fn parent_target(&self) -> Option<&VPath> {
        self.has_parent_row().then(|| &self.entries[0].path)
    }

    /// Is there a parent row RIGHT NOW in `entries`?
    ///
    /// It is a field and not a path comparison: a real entry can point at
    /// the same place as the parent —a link, a mount— and asking by path
    /// would turn that entry into "the go-up row". The field says what was
    /// really inserted.
    const fn has_parent_row(&self) -> bool {
        matches!(self.parent_row, ParentRow::Present)
    }

    /// Marks the row as NOT present, without touching `entries`: for when
    /// the listing is replaced whole and whatever there was went with it.
    const fn forget_parent_row(&mut self) {
        if let ParentRow::Present = self.parent_row {
            self.parent_row = ParentRow::Requested;
        }
    }

    /// Inserts the `..` row at the front, if it should be there and is not
    /// already.
    ///
    /// Called at the END of everything that rebuilds `entries`. Its pair
    /// [`Self::remove_parent_row`] goes at the start, and the two together
    /// are what lets sorting, filtering, and filling keep working over a
    /// listing of REAL entries — a synthetic row caught in a merge-by-sort-
    /// key is a row that gets duplicated or lost.
    ///
    /// At a root it does not appear no matter how much the config turns it
    /// on: there is nowhere to go up to, and a row that leads nowhere is
    /// worse than not having it.
    fn insert_parent_row(&mut self) {
        if !matches!(self.parent_row, ParentRow::Requested) {
            return;
        }
        let Some(parent) = self.dir.parent() else {
            return;
        };
        let row = Entry {
            attrs: std::collections::BTreeMap::new(),
            path: parent,
            kind: norte_proto::EntryKind::Dir,
            // Neither size nor date: they are not this directory's, and
            // setting them would be answering for the parent without having
            // looked at it.
            size: None,
            mtime_ms: None,
        };
        // The key is computed from ITS entry, which is the invariant the
        // sort demands: a borrowed key compares wrong the moment there is a
        // tie.
        self.sort_keys.insert(0, crate::sort::sort_key(&row));
        self.entries.insert(0, row);
        self.parent_row = ParentRow::Present;
    }

    /// Removes the `..` row if it is present.
    ///
    /// At the START of whatever rebuilds the listing, so that sorting and
    /// merging only ever see real entries.
    fn remove_parent_row(&mut self) {
        if !self.has_parent_row() {
            return;
        }
        self.entries.remove(0);
        self.sort_keys.remove(0);
        self.parent_row = ParentRow::Requested;
    }

    /// Are hidden entries shown? (#107)
    #[must_use]
    pub fn show_hidden(&self) -> bool {
        self.show_hidden
    }

    /// How many entries of the current listing are SET ASIDE by hiding
    /// (#107). 0 with [`Self::show_hidden`] on. The pane's footer paints it
    /// with the same discipline as `skipped`: a listing that shows less than
    /// there is must never be silent about it.
    #[must_use]
    pub fn hidden_count(&self) -> usize {
        self.hidden_stash.len()
    }

    /// Sets hidden visibility (#107). Showing returns the stash to the
    /// listing through the SAME path as a paginated batch ([`Self::extend`]:
    /// sorted merge, cursor re-anchored by path, quick re-applied). Hiding
    /// sets the dotfiles aside, re-anchors the cursor by path (clamped if it
    /// was on one), and PRUNES their marks with [`Self::pruned_marks`]'s
    /// counter — the same rule as `refill`: a selection feeding a bulk op
    /// must never shrink silently.
    pub fn set_show_hidden(&mut self, show: bool) {
        if show == self.show_hidden {
            return;
        }
        self.show_hidden = show;
        if show {
            let stash = std::mem::take(&mut self.hidden_stash);
            self.extend(stash);
            return;
        }
        // The `..` row comes out BEFORE partitioning and comes back
        // afterwards: it is not an entry of the listing, so it is neither
        // hidden nor saved to the stash.
        self.remove_parent_row();
        let anchor = self.entries.get(self.cursor).map(|e| e.path.clone());
        let quick_prev = self.quick_selected_path();
        let mut kept = Vec::with_capacity(self.entries.len());
        let mut kept_keys = Vec::with_capacity(self.sort_keys.len());
        // Partitions while keeping `sort_keys` index-parallel (#54): a
        // retain over `entries` alone would misalign them.
        for (entry, key) in std::mem::take(&mut self.entries)
            .into_iter()
            .zip(std::mem::take(&mut self.sort_keys))
        {
            if is_hidden_entry(&entry) {
                self.hidden_stash.push(entry);
            } else {
                kept.push(entry);
                kept_keys.push(key);
            }
        }
        self.entries = kept;
        self.sort_keys = kept_keys;
        self.insert_parent_row();
        self.listing_moved();
        self.cursor = anchor
            .and_then(|p| self.entries.iter().position(|e| e.path == p))
            .unwrap_or_else(|| self.cursor.min(self.entries.len().saturating_sub(1)));
        self.sweep_baseline = None;
        self.sweep_extent = None;
        self.pruned_marks = self.prune_marks();
        if let Some(q) = &mut self.quick {
            q.refresh(&self.entries, quick_prev.as_ref());
        }
        self.quick_sync_jump();
    }

    /// The listing's active sort order (#108).
    #[must_use]
    pub fn sort(&self) -> crate::sort::SortSpec {
        self.sort.clone()
    }

    /// Changes the listing's sort order (#108 L7): re-sorts IN PLACE (#54
    /// keys kept — only the comparator changes), re-anchors the cursor to
    /// the selected PATH, and re-applies the live quick search. Marks are
    /// not touched (they go by identity). No-op if the spec does not
    /// change.
    pub fn set_sort(&mut self, spec: crate::sort::SortSpec) {
        if spec == self.sort {
            return;
        }
        self.sort = spec;
        // The `..` row comes out BEFORE sorting and comes back afterwards:
        // it does not take part in the order, it always goes first. Sorting
        // it with the rest would send it to the middle of the listing the
        // moment someone sorts by size.
        self.remove_parent_row();
        let anchor = self.entries.get(self.cursor).map(|e| e.path.clone());
        let quick_prev = self.quick_selected_path();
        // Same anti-truncation guard as merge_keyed_spec (review m1): a zip
        // of out-of-sync parallel vecs would LOSE entries silently.
        debug_assert_eq!(
            self.entries.len(),
            self.sort_keys.len(),
            "entries and sort_keys are out of sync"
        );
        let mut pairs: Vec<(Entry, crate::sort::SortKey)> = std::mem::take(&mut self.entries)
            .into_iter()
            .zip(std::mem::take(&mut self.sort_keys))
            .collect();
        pairs.sort_by(|a, b| crate::sort::cmp_keyed_with((&a.1, &a.0), (&b.1, &b.0), &self.sort));
        (self.entries, self.sort_keys) = pairs.into_iter().unzip();
        self.insert_parent_row();
        self.listing_moved();
        self.cursor = anchor
            .and_then(|p| self.entries.iter().position(|e| e.path == p))
            .unwrap_or_else(|| self.cursor.min(self.entries.len().saturating_sub(1)));
        if let Some(q) = &mut self.quick {
            q.refresh(&self.entries, quick_prev.as_ref());
        }
        self.quick_sync_jump();
    }

    /// Toggle of [`Self::set_show_hidden`]; returns the new state.
    pub fn toggle_hidden(&mut self) -> bool {
        self.set_show_hidden(!self.show_hidden);
        self.show_hidden
    }

    /// Sets aside the hidden ones from `entries` into the stash if hiding is
    /// on (#107); passthrough otherwise. For the INGESTION points
    /// ([`Self::set_listing`], [`Self::extend`], [`Self::refill`]).
    fn stash_hidden(&mut self, entries: Vec<Entry>) -> Vec<Entry> {
        if self.show_hidden {
            return entries;
        }
        let (hidden, visible): (Vec<Entry>, Vec<Entry>) =
            entries.into_iter().partition(is_hidden_entry);
        self.hidden_stash.extend(hidden);
        visible
    }

    /// The active name reinterpretation (#57): the renders paint with it
    /// ([`crate::display_name_with`]) and the quick search folds over the
    /// same text.
    #[must_use]
    pub fn name_encoding(&self) -> Option<norte_encoding::NameEncoding> {
        self.name_encoding
    }

    /// Cycles the name reinterpretation (#57): `None` → (chardetng's
    /// suggestion over the listing's non-UTF8 names, if it falls in the
    /// cycle; cp437 otherwise) → FULL CIRCLE around the cycle — every
    /// encoding reachable from any suggestion — → `None` on returning to the
    /// entry point. A live quick search RE-FOLDS over the new text (#98/F1:
    /// the filter matches against what is SEEN). Returns the label to
    /// announce (`None` = off).
    pub fn cycle_name_encoding(&mut self) -> Option<&'static str> {
        let cycle = norte_encoding::name_reinterpret_cycle();
        self.name_encoding = match self.name_encoding {
            None => {
                let raws: Vec<&[u8]> = self
                    .entries
                    .iter()
                    .filter_map(|e| e.path.file_name().map(norte_proto::Segment::as_bytes))
                    .filter(|b| std::str::from_utf8(b).is_err())
                    .collect();
                let suggested = norte_encoding::suggest_name_encoding(&raws);
                let entry = suggested
                    .and_then(|s| cycle.iter().position(|e| *e == s))
                    .unwrap_or(0);
                self.name_encoding_entry = entry;
                Some(cycle[entry])
            }
            Some(cur) => match cycle.iter().position(|e| *e == cur) {
                Some(i) => {
                    let next = (i + 1) % cycle.len();
                    // Full circle completed: turn off (the cycle always ends
                    // at off, whatever the entry suggestion was).
                    (next != self.name_encoding_entry).then(|| cycle[next])
                }
                // Value outside the cycle (impossible today): turn off.
                None => None,
            },
        };
        // #98/F1: the live quick's fold cache was folded with the previous
        // encoding — re-fold, keeping the selection.
        let prev = self.quick_selected_path();
        let enc = self.name_encoding;
        if let Some(q) = &mut self.quick {
            q.set_name_encoding(enc, &self.entries, prev.as_ref());
        }
        self.quick_sync_jump();
        self.name_encoding.map(|e| e.label())
    }

    /// Replaces the content after a cd/refresh: resets the cursor to 0,
    /// turns off `loading`, and KILLS any live quick search (it was
    /// filtering ANOTHER listing). Normalises `entries` internally (same
    /// contract as [`PaneState::new`]).
    ///
    /// After the reset, RESTORES the cursor (spec §S1) in this precedence
    /// order: (1) [`Self::set_pending_focus`] if there is a pending hint AND
    /// an `entries` entry matches its byte-exact path (CONSUMED here,
    /// whether or not it matches); (2) otherwise, the per-dir memory
    /// ([`Self::remember_cursor`]) for the new `dir`, clamped; (3) if
    /// neither applies, 0 — the behaviour it always had.
    pub fn set_listing(&mut self, dir: VPath, entries: Vec<Entry>) {
        // #107: the PREVIOUS listing's stash goes away; the new one is
        // filtered on entry if hiding is on.
        self.hidden_stash.clear();
        let entries = self.stash_hidden(entries);
        let (entries, sort_keys) = crate::sort::sort_with_keys_spec(entries, &self.sort);
        self.dir = dir;
        self.entries = entries;
        self.sort_keys = sort_keys;
        // The dir changed: the previous `..` row pointed at a different
        // parent.
        self.forget_parent_row();
        self.insert_parent_row();
        self.listing_moved();
        self.cursor = 0;
        self.loading = false;
        self.quick = None;
        self.marks.clear();
        // And the `mark.restore` snapshot (#313): its paths belong to the
        // PREVIOUS directory, and restoring them here would mark nothing or
        // —worse— mark whatever happens to be named the same.
        self.marks_previous = None;
        self.sweep_baseline = None;
        self.sweep_extent = None;
        self.pruned_marks = 0;
        // #96: the skipped count was the PREVIOUS listing's; the caller sets
        // a fresh one with `set_skipped` if its source carries it.
        self.skipped = None;
        // G3b: the decorations were the PREVIOUS listing's (keyed by
        // byte-exact `VPath` of ANOTHER dir) — a new listing invalidates
        // them.
        self.decorations.clear();
        self.plugin_columns.clear();

        // #107 review MINOR-1 (accepted): the hint is resolved against the
        // ALREADY filtered listing — coming back from inside a hidden dir
        // with hiding on loses the focus (falls back to memory/0). Fixing it
        // would require searching the stash and picking a visible neighbour;
        // a cost not paid until it actually becomes a nuisance.
        let restored = self
            .pending_focus
            .take()
            .and_then(|child| self.entries.iter().position(|e| e.path == child))
            .or_else(|| {
                self.cursor_memory
                    .iter()
                    .find(|(d, _)| *d == self.dir)
                    .map(|&(_, c)| c)
            });
        if let Some(i) = restored {
            self.set_cursor(i);
        }
    }

    /// Marks the pane as loading `dir`: empty entries, `loading=true`, no
    /// quick search. The GUI uses it to paint a cd's destination while the
    /// listing Task runs; the real listing arrives later through
    /// [`set_listing`].
    ///
    /// The cursor memory's capture point (spec §S1) for the GUI's flow:
    /// records `(old dir, old cursor)` with [`Self::remember_cursor`] BEFORE
    /// overwriting the state with the new destination — it is the only
    /// moment the old dir is still in `self.dir`. The TUI does not call this
    /// method (its `cd` waits for the whole fetch before touching the pane,
    /// see `norte-tui::app::Pane::begin_listing`), so it records at its own
    /// equivalent capture point, right before calling [`Self::set_listing`].
    ///
    /// [`set_listing`]: PaneState::set_listing
    pub fn begin_loading(&mut self, dir: VPath) {
        self.remember_cursor();
        self.dir = dir;
        self.entries = Vec::new();
        self.sort_keys = Vec::new();
        self.forget_parent_row();
        self.insert_parent_row();
        self.listing_moved();
        self.cursor = 0;
        self.loading = true;
        self.quick = None;
        self.marks.clear();
        // And the `mark.restore` snapshot (#313): its paths belong to the
        // PREVIOUS directory, and restoring them here would mark nothing or
        // —worse— mark whatever happens to be named the same.
        self.marks_previous = None;
        self.sweep_baseline = None;
        self.sweep_extent = None;
        self.pruned_marks = 0;
        self.skipped = None;
        self.hidden_stash.clear(); // #107: belonged to the previous listing
        self.decorations.clear();
        self.plugin_columns.clear();
    }

    /// Skipped from the current listing's container (#93/#96) — see the
    /// field.
    #[must_use]
    pub fn skipped(&self) -> Option<u64> {
        self.skipped
    }

    /// Sets the current listing's FRESH skipped count (#96): call after
    /// [`Self::set_listing`]/[`Self::refill`] with the value from the SAME
    /// listing response (`list_with_skipped`/`list_stream`) — never carry
    /// over a previous listing's.
    pub fn set_skipped(&mut self, skipped: Option<u64>) {
        self.skipped = skipped;
    }

    /// `path`'s plugin decoration (G3b), already sanitised — `None` if no
    /// consented decorator decorated that path, or if this page's
    /// decorations have not arrived yet (an async fetch in progress).
    #[must_use]
    pub fn decoration_for(&self, path: &VPath) -> Option<&Decoration> {
        self.decorations.get(path)
    }

    /// Whether ANY entry of the listing has an icon (ADR 0105): if so, the
    /// icon column is painted on EVERY row, with a gap on the ones that have
    /// none, so names stay aligned. With no icon at all there is no column,
    /// and the listing looks like it did before it existed.
    #[must_use]
    pub fn any_icon(&self) -> bool {
        self.decorations.values().any(|d| d.icon.is_some())
    }

    /// Installs the BATCH of decorations already resolved and sanitised
    /// (G3b): the caller calls it after a `Backend::plugin_decorate` that
    /// answers for THE SAME listing that is still active (see
    /// [`crate::merge_decorations`] to build the map from the wire) —
    /// calling it with decorations for a `dir` that is no longer current is
    /// a harmless observable no-op (keys by another dir's `VPath` simply
    /// match no visible entry), but the caller should discard a late
    /// response whose `dir` does not match the current one BEFORE calling
    /// (see the call site in each frontend).
    pub fn set_decorations(&mut self, decorations: HashMap<VPath, Decoration>) {
        self.decorations = decorations;
    }

    /// Clears the decorations (G3b): called by [`Self::set_listing`]/
    /// [`Self::begin_loading`] — also exposed so a caller can force the
    /// reset (e.g. when disabling every decorator).
    pub fn clear_decorations(&mut self) {
        self.decorations.clear();
    }

    /// Installs the BATCH of `plugin:` column values (#117-follow-up): outer
    /// key = Display id (`plugin:<p>/<c>`), inner = the active listing's
    /// `VPath` → sanitised value
    /// ([`crate::columns::sanitize_column_values`] at ingest). Same
    /// anti-staleness contract as [`Self::set_decorations`]: the caller
    /// discards a late response whose `dir` does not match the current one.
    pub fn set_plugin_columns(&mut self, columns: HashMap<String, HashMap<VPath, String>>) {
        self.plugin_columns = columns;
    }

    /// The `plugin:` column `display_id`'s cell for `path` (#117-follow-up):
    /// `None` = no value (blank, never fabricated). The value is
    /// defensively RE-masked when served (P1 doctrine: an unmasked bidi
    /// character in ratatui DISAPPEARS silently — consumers do not trust
    /// that the ingest already sanitised it).
    #[must_use]
    pub fn plugin_cell(&self, display_id: &str, path: &VPath) -> Option<String> {
        let v = self.plugin_columns.get(display_id)?.get(path)?;
        crate::columns::sanitize_cell(Some(v))
    }

    /// Moves the cursor up one position (stops at 0). No-op if the list is
    /// empty.
    pub fn cursor_up(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    /// Moves the cursor down one position (stops at the last entry). No-op
    /// if empty.
    pub fn cursor_down(&mut self) {
        let max = self.entries.len().saturating_sub(1);
        self.cursor = (self.cursor + 1).min(max);
    }

    /// Moves the cursor up `n` positions (stops at 0).
    pub fn page_up(&mut self, n: usize) {
        self.cursor = self.cursor.saturating_sub(n);
    }

    /// Moves the cursor down `n` positions (stops at the last entry).
    pub fn page_down(&mut self, n: usize) {
        let max = self.entries.len().saturating_sub(1);
        self.cursor = (self.cursor + n).min(max);
    }

    /// Cursor to the first entry.
    pub fn home(&mut self) {
        self.cursor = 0;
    }

    /// Cursor to the last entry (0 if the list is empty).
    pub fn end(&mut self) {
        self.cursor = self.entries.len().saturating_sub(1);
    }

    /// The selected entry: with a quick search in [`Mode::Filter`] mode, the
    /// selection WITHIN the filter (so ops act on the filtered set without
    /// knowing about the quick search); if the filter has no matches, `None`
    /// (never an entry the user does not see). With no filter (or in
    /// [`Mode::Jump`], which moves the real cursor), the entry under the
    /// cursor.
    #[must_use]
    pub fn selected(&self) -> Option<&Entry> {
        // The `..` row is NOT an operand, and this is THE place that decides
        // it.
        //
        // Eighty-seven call sites ask for "what is pointed at" to copy it,
        // delete it, rename it, or look inside it, and none of them has any
        // business knowing that a row exists that is not a file. Answering
        // `None` —which all of them already know how to handle: it is "there
        // is nothing selected"— makes the row harmless by construction,
        // instead of by everyone remembering to check.
        //
        // Going up with it does not go through here: that is
        // `parent_target`, watched by whoever navigates; describing it does
        // not either, and that is [`Self::cursor_entry`].
        //
        // The guard is on the POINTED-AT INDEX and not on `self.cursor`, and
        // that fixes a hole that had been here from the start: in
        // `Mode::Filter` the quick search's selection rules and the real
        // cursor does not move, so opening the filter —whose empty query is
        // born pointing at row 0— left `selected()` returning the PARENT
        // directory. F8 there deletes the parent, which is exactly what this
        // row exists to prevent.
        if self.is_parent_row(self.pointed_index()?) {
            return None;
        }
        self.pointed()
    }

    /// The entry under the cursor TO DESCRIBE IT, `..` row included.
    ///
    /// [`Self::selected`] answers "what does this act on" and that is why it
    /// stays silent about the go-up row. This one answers "what is being
    /// pointed at", which is a different question with a different answer:
    /// the panels that follow the cursor —the attribute sheet, the docked
    /// viewer— DESCRIBE whatever is underneath.
    ///
    /// Asking the first one used to say "nothing under the cursor" while a
    /// row sat right there, and since the cursor is born over `..`, the
    /// details panel started empty on every open and after every `cd`.
    ///
    /// **It is not an operand.** Whoever copies, deletes, renames, or looks
    /// inside asks [`Self::selected`]; this one is only good for painting.
    #[must_use]
    pub fn cursor_entry(&self) -> Option<&Entry> {
        self.pointed()
    }

    /// Is what is pointed at RIGHT NOW the `..` row?
    ///
    /// The question that goes with [`Self::cursor_entry`]: whoever describes
    /// it needs to know it is, because the synthetic `Entry` carries the
    /// PARENT's path and describing it by its `file_name` would assert that
    /// the cursor is over the parent.
    ///
    /// It comes from the SAME index as the entry, and that is why it exists:
    /// asking `is_parent_row(cursor())` separately, a quick search filter
    /// —which chooses on its own and does not move the real cursor— left the
    /// flag and the entry talking about different rows.
    #[must_use]
    pub fn cursor_is_parent_row(&self) -> bool {
        self.pointed_index().is_some_and(|i| self.is_parent_row(i))
    }

    /// The row the cursor —or the quick search's filter— is pointing at,
    /// without the `..` row's guard.
    fn pointed(&self) -> Option<&Entry> {
        self.entries.get(self.pointed_index()?)
    }

    /// The pointed-at INDEX: the filter's selection when there is one, and
    /// the real cursor when there is not.
    ///
    /// ONE answer, and the three questions come from it —"what does this
    /// act on", "what is pointed at", and "is it the go-up row?". Three
    /// independent calculations of the same thing is how two of them end up
    /// talking about different rows.
    fn pointed_index(&self) -> Option<usize> {
        if let Some(q) = &self.quick
            && q.mode() == Mode::Filter
        {
            return q.selected_entry_index();
        }
        Some(self.cursor)
    }

    /// The listed directory.
    #[must_use]
    pub fn dir(&self) -> &VPath {
        &self.dir
    }

    /// Current entries (sorted by the caller), **`..` row included**.
    ///
    /// It is the list that gets PAINTED, and that is why it carries it: the
    /// indices here are the ones [`Self::is_parent_row`] answers about and
    /// the ones the cursor, the mouse, and marking use. What gets copied to
    /// ANOTHER pane is [`Self::real_entries`].
    #[must_use]
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// POINTS AT entry `entry`, whoever is in charge of the selection.
    ///
    /// It is the write twin of "the pointed-at index", and exists because
    /// [`Self::set_cursor`] is NOT enough: with a live quick search in
    /// [`Mode::Filter`], what is pointed at is the filter's selection and the
    /// real cursor is not looked at, so moving the cursor leaves everything
    /// that follows "what is pointed at" —the docked viewer, the attribute
    /// sheet— standing still while the gesture claims it worked.
    ///
    /// With a filter it moves the quick's selection, step by step through
    /// what is VISIBLE; without one, the cursor. An `entry` the filter does
    /// not show cannot be pointed at: nothing is touched.
    pub fn senalar(&mut self, entry: usize) {
        let Some(vis) = self.quick_visible() else {
            self.set_cursor(entry);
            return;
        };
        let target = vis.iter().position(|&real| real == entry);
        let current = self
            .quick()
            .and_then(QuickSearch::selected_entry_index)
            .and_then(|real| vis.iter().position(|&r| r == real));
        let (Some(current), Some(target)) = (current, target) else {
            return;
        };
        // No signed subtraction: the direction is a boolean and the distance
        // a count, which is exactly what `quick_down`/`quick_up` consume.
        let (forward, steps) = if target >= current {
            (true, target - current)
        } else {
            (false, current - target)
        };
        for _ in 0..steps {
            if forward {
                self.quick_down();
            } else {
                self.quick_up();
            }
        }
    }

    /// Where a gesture that adopts the CURSOR'S TARGET points: the folder
    /// under the cursor if it is one, and this pane's directory otherwise.
    ///
    /// It is Krusader's `Ctrl+←`/`Ctrl+→` rule, literally: "on a folder:
    /// refreshes the other panel with the contents of the folder; on a file:
    /// the other panel gets the same path". Lives here, and not in each
    /// frontend, because a decision duplicated between the two diverges
    /// silently (ADR 0077).
    ///
    /// Over the `..` row it returns this pane's directory, not the parent:
    /// [`Self::selected`] answers `None` there —it is the choke point that
    /// stops that row from being anything's operand— and this gesture is no
    /// exception. A link to a directory does not count either: in M0 a
    /// symlink is not followed, and sending the other panel to where it
    /// points would be following it.
    #[must_use]
    pub fn target_dir(&self) -> &VPath {
        match self.selected() {
            Some(e) if e.kind == EntryKind::Dir => &e.path,
            _ => &self.dir,
        }
    }

    /// The REAL entries: [`Self::entries`] without the `..` row.
    ///
    /// What has to be copied when a pane is born from another one's listing
    /// —splitting a panel, opening a tab— because the new pane sets up its
    /// own. With `entries()` the inherited one stayed as a normal entry in
    /// the middle of the listing, with the parent directory's name and
    /// markable: every split added one, and marking everything swept the
    /// PARENT into what gets copied or deleted.
    ///
    /// The field decides it, not the path, for the same reason as
    /// [`Self::is_parent_row`]: a real entry can point at the same place as
    /// the parent.
    #[must_use]
    pub fn real_entries(&self) -> &[Entry] {
        if self.has_parent_row() {
            &self.entries[1..]
        } else {
            &self.entries
        }
    }

    /// The names a plan producer can ORGANIZE (phase 8): this directory's
    /// files, in text.
    ///
    /// Three filters, and each one covers a hole seen while piloting:
    ///
    /// - **No `..` row** — it comes from [`Self::real_entries`]. Reading
    ///   `entries()` raw, an organizer would propose moving the PARENT
    ///   DIRECTORY into a new folder; it is the same trap `real_entries`
    ///   already documents, and the reason this lives here and not in each
    ///   frontend.
    /// - **No directories.** Organizing means filing FILES into folders.
    ///   Letting directories in lets the plan move a folder that another
    ///   move of the same plan uses as a destination: the tree shows
    ///   `pdf/a.pdf`, and once applied `pdf` has moved elsewhere with `a.pdf`
    ///   inside it. Nothing is lost, but what got applied is not what was
    ///   reviewed, which is worse.
    /// - **Only what is text.** `proposed_rel` travels as UTF-8, so a name
    ///   that is not cannot be the source of a move. It stays out instead of
    ///   travelling lossy and coming back pointing at a different file.
    #[must_use]
    pub fn organizable_names(&self) -> Vec<String> {
        self.real_entries()
            .iter()
            .filter(|e| e.kind != norte_proto::EntryKind::Dir)
            .filter_map(|e| e.path.file_name())
            .filter_map(|s| std::str::from_utf8(s.as_bytes()).ok().map(str::to_owned))
            .collect()
    }

    /// The names that already OCCUPY this directory, in text: what the
    /// organize tree needs to tell a new folder apart from one that was
    /// already there.
    ///
    /// Here directories DO get in —they are exactly the ones that matter—
    /// and the `..` row stays out: the parent is not an entry of this
    /// directory, and counting it would make a folder named the same as it
    /// "existing".
    #[must_use]
    pub fn existing_names(&self) -> Vec<String> {
        self.real_entries()
            .iter()
            .filter_map(|e| e.path.file_name())
            .filter_map(|s| std::str::from_utf8(s.as_bytes()).ok().map(str::to_owned))
            .collect()
    }

    /// Index under the real cursor (0 even with an empty list).
    #[must_use]
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// The listing is loading (an in-progress cd's destination).
    #[must_use]
    pub fn loading(&self) -> bool {
        self.loading
    }

    /// How many times this pane's listing has MOVED (see the
    /// `listing_epoch` field). Opaque and monotonic: compare two readings,
    /// never interpret the number.
    ///
    /// The frontends hold it next to the geometry they painted, so that an
    /// in-flight pointer gesture whose indices no longer name what the user
    /// saw is dropped rather than applied to the new listing.
    ///
    /// ```
    /// use norte_frontend::PaneState;
    /// use norte_proto::VPath;
    ///
    /// let dir = VPath::parse("mem:///d").unwrap();
    /// let mut p = PaneState::new(dir.clone(), Vec::new());
    /// let before = p.listing_epoch();
    /// p.set_listing(dir, Vec::new());
    /// assert_ne!(p.listing_epoch(), before, "another listing, other indices");
    /// ```
    #[must_use]
    pub fn listing_epoch(&self) -> u64 {
        self.listing_epoch
    }

    /// Records that the indices of [`Self::entries`] may have moved.
    ///
    /// Called from EVERY site that touches `entries` — one line each,
    /// rather than a guess derived from the length (a re-sort keeps the
    /// length and moves every index) or from the directory (a refill of the
    /// same directory moves them too).
    ///
    /// Saturating: a session that overflowed a `u64` of listing changes is
    /// not reachable, and wrapping back onto the epoch a frontend is
    /// holding is the one outcome worth ruling out.
    fn listing_moved(&mut self) {
        self.listing_epoch = self.listing_epoch.saturating_add(1);
        self.name_buckets = buckets_of(&self.entries);
    }

    /// Cells that cover 80% of this listing's names: what the name needs to
    /// be readable, measured when the listing changes.
    #[must_use]
    pub fn name_width_p80(&self) -> u16 {
        crate::columns::name_width_p80(&self.name_buckets)
    }

    /// Sets the real cursor to `i`, clamped (never out of range). For
    /// re-anchoring after locating a specific index (e.g. a search hit).
    /// (#82)
    pub fn set_cursor(&mut self, i: usize) {
        let max = self.entries.len().saturating_sub(1);
        self.cursor = i.min(max);
    }

    /// Records `(current dir, current cursor)` in the cursor memory (spec
    /// §S1): session-only, per pane, LRU with a `CURSOR_MEMORY_CAP` cap
    /// (private module constant, 64).
    /// Replaces any previous entry for the same dir (byte-exact identity,
    /// not normalised — rule 1) so each dir has at most ONE entry, always
    /// the most recent.
    ///
    /// The caller must invoke it while `self.dir`/`self.cursor` STILL
    /// reflect the dir being left — before any reset (see
    /// [`Self::begin_loading`], which calls it first for that reason).
    pub fn remember_cursor(&mut self) {
        let dir = self.dir.clone();
        self.cursor_memory.retain(|(d, _)| *d != dir);
        self.cursor_memory.push((dir, self.cursor));
        if self.cursor_memory.len() > CURSOR_MEMORY_CAP {
            self.cursor_memory.remove(0);
        }
    }

    /// Sets a pending focus (spec §S1, `nav.parent`): on the NEXT
    /// [`Self::set_listing`], if an entry of the new listing has this EXACT
    /// path (bytes, not normalised — rule 1), the cursor lands there — ahead
    /// of the memory. Consumed once (whether it matches or not) so it does
    /// not leak into unrelated future navigations.
    pub fn set_pending_focus(&mut self, child: VPath) {
        self.pending_focus = Some(child);
    }

    /// Discards a pending focus WITHOUT consuming it against a listing
    /// (review S, M2): [`Self::set_listing`] was, until now, the ONLY place
    /// that consumed `pending_focus` — a `nav.parent` whose `cd` FAILS
    /// (permission denied, daemon error…) never reaches `set_listing`, so
    /// the hint stayed alive and could land on a MUCH later, unrelated `cd`,
    /// in the wrong pane. The caller (`nav.parent`, both frontends) calls
    /// this on the `cd`'s error branch.
    pub fn clear_pending_focus(&mut self) {
        self.pending_focus = None;
    }

    /// Marks/unmarks the pane as loading WITHOUT touching the rest of the
    /// state: a paginated fill (ADR 0017) paints the first page and keeps
    /// going (`true`), then drops the flag when it finishes (`false`). (#82)
    pub fn set_loading(&mut self, loading: bool) {
        self.loading = loading;
    }

    /// Adds `batch` to an in-progress paginated listing (ADR 0017): #54
    /// merges O(n+m) with the PERSISTED NFC keys (`sort_keys`) — the batch
    /// sorts itself and merges stably against what was already listed, the
    /// same final order as [`sort_entries`](crate::sort_entries) over the
    /// whole thing, without recomputing what was already there's key.
    /// Reconciles: re-anchors the cursor to the selected PATH (clamped by
    /// index if it disappeared) and RE-APPLIES the quick search by path. An
    /// empty batch is a no-op. (#82)
    pub fn extend(&mut self, batch: Vec<Entry>) {
        // #107: the batch's hidden ones are set aside BEFORE the merge — a
        // batch left empty after the filter still feeds the stash.
        let batch = self.stash_hidden(batch);
        if batch.is_empty() {
            return;
        }
        let quick_prev = self.quick_selected_path();
        // The cursor AT THE TOP anchors to its POSITION, not the path: a
        // paginated dir's first page arrives in `readdir` order (FS hash),
        // so its first element once sorted is arbitrary. Anchoring it by
        // path pinned the cursor in the middle of the final listing —a
        // 5000-entry dir opened showing the TAIL— even though the user had
        // touched nothing. As soon as it moves the cursor, anchoring by
        // path takes over again (filling must not move its selection out
        // from under it).
        let anchor = (self.cursor > 0)
            .then(|| self.entries.get(self.cursor).map(|e| e.path.clone()))
            .flatten();
        // The `..` row comes out before the MERGE: the merge pairs by sort
        // key, and a synthetic row caught in there would duplicate or end
        // up in the middle of the listing.
        self.remove_parent_row();
        // The batch's names are ADDED to the histogram: re-measuring the
        // whole listing on every page would make the fill quadratic when the
        // merge keeps it linear.
        measure_names(&mut self.name_buckets, &batch);
        let (batch, batch_keys) = crate::sort::sort_with_keys_spec(batch, &self.sort);
        crate::sort::merge_keyed_spec(
            &mut self.entries,
            &mut self.sort_keys,
            batch,
            batch_keys,
            &self.sort,
        );
        self.insert_parent_row();
        // A page of a paginated fill also MOVES indices: the merge inserts
        // in its sorted spot, not at the end. Only the epoch: the names were
        // already added above.
        self.listing_epoch = self.listing_epoch.saturating_add(1);
        self.cursor = anchor
            .and_then(|p| self.entries.iter().position(|e| e.path == p))
            .unwrap_or_else(|| self.cursor.min(self.entries.len().saturating_sub(1)));
        if let Some(q) = &mut self.quick {
            q.refresh(&self.entries, quick_prev.as_ref());
        }
        self.quick_sync_jump();
    }

    /// Replaces the COMPLETE listing of the SAME dir (refresh after a
    /// mutation): keeps the cursor by clamped INDEX (after a delete it lands
    /// on the next entry — orthodox) and RE-APPLIES the quick search by
    /// path. Does not touch the loading flag. Normalises `entries`
    /// internally (#54: closes the same footgun as `new`/`set_listing` —
    /// idempotent if the caller was already sorted). (#82)
    ///
    /// Marks are pruned to the paths present in `entries` (#103, see
    /// `prune_marks`/[`Self::pruned_marks`]) — the listing passed in
    /// must be COMPLETE, since a partial page would silently discard the
    /// marks it omits.
    pub fn refill(&mut self, mut entries: Vec<Entry>) {
        let quick_prev = self.quick_selected_path();
        self.inherit_known_metadata(&mut entries);
        // #107: the refill brings the dir's COMPLETE listing — the stash is
        // rebuilt fresh from it, never accumulated with the previous one.
        self.hidden_stash.clear();
        let entries = self.stash_hidden(entries);
        let (entries, sort_keys) = crate::sort::sort_with_keys_spec(entries, &self.sort);
        self.entries = entries;
        self.sort_keys = sort_keys;
        // The listing was rebuilt whole: the row comes back, and the cursor
        // is clamped AFTER inserting it —otherwise, with a listing that
        // shrank, it would end up one row above what there actually is.
        self.forget_parent_row();
        self.insert_parent_row();
        self.cursor = self.cursor.min(self.entries.len().saturating_sub(1));
        self.listing_moved();
        self.sweep_baseline = None;
        self.sweep_extent = None;
        self.pruned_marks = self.prune_marks();
        if let Some(q) = &mut self.quick {
            q.refresh(&self.entries, quick_prev.as_ref());
        }
        self.quick_sync_jump();
    }

    /// Carries over to `entries` the size/mtime this pane ALREADY knew for
    /// the same path, only where the new listing does not bring it.
    ///
    /// The reason is visual and concrete: a local `fs.list` does not `stat`
    /// every entry (#52), so a refresh of the SAME dir arrives bare and,
    /// installed as-is, empties the size and date columns until the probe
    /// fills them in. With the watcher (#106) refreshing on every directory
    /// event, that looks like continuous flicker. Inheriting freezes
    /// nothing: a listing that DOES bring the data wins, and
    /// [`Self::hydrate`] —the probe— wins over both.
    fn inherit_known_metadata(&self, entries: &mut [Entry]) {
        // Hidden ones count: the hiding toggle returns them to the listing
        // and they would lose the data if we only looked at the visible
        // ones.
        let known: HashMap<&VPath, (Option<u64>, Option<i64>)> = self
            .entries
            .iter()
            .chain(self.hidden_stash.iter())
            .filter(|e| e.size.is_some() || e.mtime_ms.is_some())
            .map(|e| (&e.path, (e.size, e.mtime_ms)))
            .collect();
        if known.is_empty() {
            return;
        }
        for entry in entries {
            if let Some(&(size, mtime_ms)) = known.get(&entry.path) {
                entry.size = entry.size.or(size);
                entry.mtime_ms = entry.mtime_ms.or(mtime_ms);
            }
        }
    }

    /// Hydrates the size/mtime of entry `path` (stat on-demand, #52). No-op
    /// if the entry is no longer there (a refresh overwrote it). Does not
    /// re-sort: size/mtime take no part in the sort.
    ///
    /// The probe is AUTHORITATIVE: it just looked at the file, so its value
    /// overwrites whatever there was (which can be inherited from before the
    /// refresh, see `inherit_known_metadata` — without this, a growing file
    /// would forever show the size it was listed with the first time). What
    /// it does NOT overwrite with is `None`: a failed stat or a provider
    /// that does not know the data never erases one that was known.
    /// (#107 review MINOR-5, accepted: a stat that resolves after its entry
    /// moved to the hidden stash is lost — on showing it again, the row
    /// paints `None` until the next focus probe. Self-healing and cheap.)
    pub fn hydrate(&mut self, path: &VPath, size: Option<u64>, mtime_ms: Option<i64>) {
        if let Some(e) = self.entries.iter_mut().find(|e| &e.path == path) {
            e.size = size.or(e.size);
            e.mtime_ms = mtime_ms.or(e.mtime_ms);
        }
    }
}

#[cfg(test)]
mod tests;
