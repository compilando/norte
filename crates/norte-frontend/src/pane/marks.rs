//! The marks: what is chosen to operate on, and the sweep that paints them
//! with the mouse.
//!
//! It is the half of the pane that decides WHAT an operation acts on, so it
//! lives together: marking one by one, all of them, by pattern, inverting,
//! and the sweep —which is a TENTATIVE edit with its baseline, so releasing
//! the button where you started does not leave half a selection made.

use super::{
    Entry, EntryKind, GlobBuilder, HashSet, Mode, PaneState, PatternError, VPath,
    unicode_glob_regex,
};

/// What is known about a listing's marks in a single pass
/// ([`PaneState::marks_summary`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MarksSummary {
    /// What the marked FILES that declare a size weigh.
    pub bytes: u64,
    /// How many of the marked ones are directories.
    pub dirs: usize,
    /// The mark ruler: the segments carrying any, in order.
    pub ruler: Vec<u16>,
}

/// A BASE name's extension, in bytes and without the dot; `None` if it has
/// none.
///
/// Same rule as [`crate::rename_pattern::split_name`], and on purpose: the
/// separating dot is the LAST one, and a name that starts with a dot and has
/// no other —`.bashrc`— has no extension, it has a name. Two definitions of
/// "the extension" in the same program would end up marking one set and
/// renaming another.
///
/// In bytes because a name need not be text (rule 1): passing it through
/// `String` would make two different names that collapse to the same
/// replacement character get marked together.
fn extension_of(name: &[u8]) -> Option<&[u8]> {
    match name.iter().rposition(|b| *b == b'.') {
        Some(0) | None => None,
        Some(i) => Some(&name[i + 1..]),
    }
}

impl PaneState {
    /// The ONLY door through which a path enters the mark set. `true` = it
    /// was not there and now it is.
    ///
    /// Rejects the PARENT directory's path, and that is the safety net under
    /// everything else: the main defense is that the `..` row is not
    /// markable (through `markable_indices`) and is not copied to another
    /// pane (through [`PaneState::real_entries`]) — this one turns the next
    /// slip into "nothing happens" instead of deleting the directory above,
    /// which is what happened when splitting a panel copied it as a normal
    /// entry.
    ///
    /// Looking at the PATH is safe HERE and would still not be safe in
    /// [`PaneState::is_parent_row`]: an entry's path in this directory is
    /// always `dir/name`, so only the synthetic row —or a copy of it— can be
    /// exactly the parent; a link or a mount that POINTS at the parent has
    /// its own and gets marked like any other.
    fn mark(&mut self, path: VPath) -> bool {
        if self.parent_target() == Some(&path) {
            return false;
        }
        self.marks.insert(path)
    }

    /// Seeds the marks carried over by a hand-off between frontends (phase
    /// 9).
    ///
    /// It can be called BEFORE the listing arrives, and that is the normal
    /// case: the pane comes up on the saved path and its entries drain in
    /// afterwards. The marks live in a set of `VPath`, so seeding them early
    /// does not depend on the row existing yet — when it arrives, it will
    /// show up marked.
    ///
    /// A path that is no longer in the directory stays in the set and does
    /// nothing: [`Self::marked_paths`] filters by the entries, so it cannot
    /// be an operation's operand. And the `..` row does not get in, through
    /// the same door as everything else.
    pub fn seed_marks(&mut self, paths: impl IntoIterator<Item = VPath>) {
        for p in paths {
            self.mark(p);
        }
    }

    /// Toggles the selected entry's mark (respects the quick filter: marks
    /// the VISIBLE entry under the selection). No-op if there is no
    /// selection.
    pub fn toggle_mark(&mut self) {
        let Some(path) = self.selected().map(|e| e.path.clone()) else {
            return;
        };
        if !self.marks.remove(&path) {
            self.mark(path);
        }
    }

    /// mc/Total Commander sweep (`insert`, #103): toggle-mark the VISIBLE
    /// selection, then advance to the next visible row — holding the key
    /// selects a range. With a [`Mode::Filter`] quick search active, "next"
    /// means the next VISIBLE row within the filter ([`Self::quick_down`],
    /// which does not wrap); the real cursor is left untouched, exactly as
    /// [`Self::toggle_mark`] itself only ever acts on the filtered
    /// selection. Without an active filter (or in [`Mode::Jump`], where
    /// [`Self::selected`] already reads the real cursor), it advances the
    /// real cursor ([`Self::page_down`], which clamps). Either way, at the
    /// last visible row this marks WITHOUT wrapping back to the top.
    pub fn toggle_mark_and_advance(&mut self) {
        self.toggle_mark();
        let filtering = self
            .quick
            .as_ref()
            .is_some_and(|q| q.mode() == Mode::Filter);
        if filtering {
            self.quick_down();
        } else {
            self.page_down(1);
        }
    }

    /// The index of the row under the cursor IN `entries`, honouring the
    /// filter.
    ///
    /// It is the coordinate `mark_range` and `set_mark` speak in, and it is
    /// not the same as `cursor()` when a quick filter is on.
    fn index_under_cursor(&self) -> Option<usize> {
        if let Some(q) = &self.quick
            && q.mode() == Mode::Filter
        {
            return q.selected_entry_index();
        }
        self.is_markable(self.cursor).then_some(self.cursor)
    }

    /// Toggle-and-move UPWARD: the mirror of
    /// [`Self::toggle_mark_and_advance`] (Far's and everyone else's
    /// `shift+↑`).
    ///
    /// Exists because the family was half-done: `space`/`insert` mark going
    /// down, and there was no way to mark going up — a reader who went one
    /// row too far had to go back up, unmark by hand, and return.
    pub fn toggle_mark_and_retreat(&mut self) {
        self.toggle_mark();
        let filtering = self
            .quick
            .as_ref()
            .is_some_and(|q| q.mode() == Mode::Filter);
        if filtering {
            self.quick_up();
        } else {
            self.page_up(1);
        }
    }

    /// Applies to the WHOLE stretch between the cursor and `n` rows below (or
    /// above, with `downward` false) the opposite of whatever the cursor's
    /// row has, and leaves the cursor at the end of the stretch.
    ///
    /// It is the CURSOR's row that decides, not each row: that way the
    /// gesture is reversible —repeating it undoes what it did— and a
    /// half-made selection does not keep flip-flopping. It is Far's and
    /// Total Commander's `shift+PgDn`/`shift+PgUp` rule, and the same one
    /// that makes "to deselect, move in the opposite direction" make sense.
    ///
    /// With a quick filter on it does not leave the VISIBLE set: `mark_range`
    /// and `set_mark` already honour it, and the cursor moves along the
    /// filtered path.
    pub fn toggle_mark_page(&mut self, n: usize, downward: bool) {
        let Some(from) = self.index_under_cursor() else {
            return;
        };
        let should_mark = !self.marks.contains(&self.entries[from].path);
        self.snapshot_marks();
        // Move FIRST and read the destination afterwards: how far it really
        // advances is decided by the pane (limits, filter), and assuming it
        // here would mark a stretch the cursor does not travel through.
        let filtering = self
            .quick
            .as_ref()
            .is_some_and(|q| q.mode() == Mode::Filter);
        for _ in 0..n {
            match (filtering, downward) {
                (true, true) => self.quick_down(),
                (true, false) => self.quick_up(),
                (false, true) => self.page_down(1),
                (false, false) => self.page_up(1),
            }
        }
        let to = self.index_under_cursor().unwrap_or(from);
        if should_mark {
            self.mark_range(from, to);
        } else {
            let (a, b) = if from <= to { (from, to) } else { (to, from) };
            for i in self.markable_indices() {
                if (a..=b).contains(&i) {
                    self.set_mark(i, false);
                }
            }
        }
    }

    /// Krusader `Shift+Home`: marks everything from the cursor UPWARD and
    /// UNMARKS whatever is left below.
    ///
    /// The second half is not an extra: it is what Krusader's documentation
    /// says literally ("selects everything above the cursor **and
    /// deselects everything below the cursor, if selected**"), and it is
    /// what tells this gesture apart from an "add a stretch". Without it, a
    /// reader using it to bound a selection would sweep away what they
    /// thought they had left out.
    pub fn mark_to_top(&mut self) {
        self.mark_to_edge(true);
    }

    /// Krusader `Shift+End`: marks from the cursor DOWNWARD and unmarks
    /// what is above. The mirror of [`Self::mark_to_top`].
    pub fn mark_to_bottom(&mut self) {
        self.mark_to_edge(false);
    }

    /// The body of the two above: `upward` picks which side gets marked.
    ///
    /// The cursor does NOT move. Krusader does not move it either, and here
    /// it matters more: the stretch is defined from where it is, so moving
    /// it would leave the reader without the point they just bounded from.
    fn mark_to_edge(&mut self, upward: bool) {
        let Some(from) = self.index_under_cursor() else {
            return;
        };
        self.snapshot_marks();
        for i in self.markable_indices() {
            let inside = if upward { i <= from } else { i >= from };
            self.set_mark(i, inside);
        }
    }

    /// Is this entry marked? (by its absolute `VPath`).
    #[must_use]
    pub fn is_marked(&self, entry: &Entry) -> bool {
        self.marks.contains(&entry.path)
    }

    /// How many marked entries there are.
    #[must_use]
    pub fn marks_len(&self) -> usize {
        self.marks.len()
    }

    /// What a listing's header says about its marks, in ONE pass: how much
    /// the marked files weigh, how many directories are among them, and the
    /// mark ruler over `segments` segments.
    ///
    /// It is [`Self::marked_bytes`], [`Self::marked_dirs`] and
    /// [`Self::mark_ruler`] together, and that is why it exists: the three
    /// used to walk the whole listing each, and the header asks for them on
    /// every keystroke — marking in a directory of twenty thousand entries
    /// cost three passes per keypress.
    #[must_use]
    pub fn marks_summary(&self, segments: u16) -> MarksSummary {
        let mut out = MarksSummary::default();
        let total = self.entries.len();
        if self.marks.is_empty() || total == 0 {
            return out;
        }
        for (i, e) in self.entries.iter().enumerate() {
            if !self.marks.contains(&e.path) {
                continue;
            }
            if e.kind == EntryKind::Dir {
                out.dirs += 1;
            } else {
                out.bytes = out.bytes.saturating_add(e.size.unwrap_or(0));
            }
            if segments > 0 {
                // `i < total`, so the quotient is `< segments`.
                let segment =
                    u16::try_from(i * usize::from(segments) / total).unwrap_or(segments - 1);
                if out.ruler.last() != Some(&segment) {
                    out.ruler.push(segment);
                }
            }
        }
        out
    }

    /// Which segments of the listing carry any mark, for the ruler next to
    /// the window's scrollbar (ADR 0135).
    ///
    /// The listing is split into `segments` equal chunks by POSITION in
    /// `entries` —the same space as `total_rows`— and the indices of the
    /// ones that have at least one mark are returned, in order and without
    /// repeats. Bounded by `segments` and not by the number of marks: ten
    /// thousand marked entries do not cross the bridge as ten thousand
    /// numbers. Empty if there are no marks or `segments` is zero.
    #[must_use]
    pub fn mark_ruler(&self, segments: u16) -> Vec<u16> {
        let total = self.entries.len();
        if self.marks.is_empty() || total == 0 || segments == 0 {
            return Vec::new();
        }
        let mut out: Vec<u16> = Vec::new();
        for (i, e) in self.entries.iter().enumerate() {
            if !self.marks.contains(&e.path) {
                continue;
            }
            // `i < total`, so the quotient is `< segments` and fits in u16.
            let segment = u16::try_from(i * usize::from(segments) / total).unwrap_or(segments - 1);
            if out.last() != Some(&segment) {
                out.push(segment);
            }
        }
        out
    }

    /// The MARKED entries, without falling back to the cursor when there are
    /// none.
    ///
    /// This is what whoever has to tell "no marks" apart from "there is one"
    /// needs: [`Self::marked_paths`] returns the cursor in the first case,
    /// which is right for copying and wrong for a rule that requires exactly
    /// two (#312). Entries and not paths because that rule also looks at
    /// `kind`.
    #[must_use]
    pub fn marked_entries(&self) -> Vec<&Entry> {
        self.entries
            .iter()
            .filter(|e| self.marks.contains(&e.path))
            .collect()
    }

    /// The `VPath`s the action operates on: the marks (in `entries`'
    /// ORDER, deterministic), or the selection (honours the quick filter) if
    /// there are no marks (empty if there is no selection either). The single
    /// source for "what the op acts on".
    #[must_use]
    pub fn marked_paths(&self) -> Vec<VPath> {
        if self.marks.is_empty() {
            return self
                .selected()
                .map(|e| e.path.clone())
                .into_iter()
                .collect();
        }
        self.entries
            .iter()
            .filter(|e| self.marks.contains(&e.path))
            .map(|e| e.path.clone())
            .collect()
    }

    /// Saves the CURRENT selection as the one `mark.restore` returns.
    ///
    /// Every BULK gesture calls it, and only those: marking or unmarking a
    /// row by hand loses nothing worth rescuing, and saving a snapshot per
    /// keystroke would leave "restore" meaning "undo the last key", which is
    /// a different function and not the one TC has.
    fn snapshot_marks(&mut self) {
        self.marks_previous = Some(self.marks.clone());
    }

    /// Returns the selection from before the last bulk operation (#313), and
    /// leaves the current one as the new "previous".
    ///
    /// Returns how many entries remain marked, or `None` if there is no
    /// snapshot — nothing to restore, and the caller says so instead of
    /// leaving the panel with no marks while pretending that was the earlier
    /// state.
    ///
    /// It goes and it COMES BACK on purpose: what rescues someone who pressed
    /// "unmark all" by accident also has to rescue someone who pressed
    /// "restore" by accident. Only the paths still in the listing are kept,
    /// with the same byte-exact identity as always.
    ///
    /// ```
    /// use norte_frontend::PaneState;
    /// # use norte_proto::{Entry, EntryKind, VPath};
    /// # let dir = VPath::parse("mem:///d").unwrap();
    /// # fn e(dir: &VPath, n: &str) -> Entry {
    /// #     Entry { path: dir.join(norte_proto::Segment::new(n.as_bytes().to_vec()).unwrap()),
    /// #             kind: EntryKind::File, size: None, mtime_ms: None, attrs: Default::default() }
    /// # }
    /// let mut p = PaneState::new(dir.clone(), vec![e(&dir, "a"), e(&dir, "b")]);
    /// assert_eq!(p.restore_previous_marks(), None, "there is nothing to restore yet");
    /// p.mark_all();
    /// p.clear_marks();
    /// assert_eq!(p.marks_len(), 0);
    /// assert_eq!(p.restore_previous_marks(), Some(2), "both come back");
    /// assert_eq!(p.restore_previous_marks(), Some(0), "and restoring undoes itself");
    /// ```
    pub fn restore_previous_marks(&mut self) -> Option<usize> {
        let previous = self.marks_previous.take()?;
        let current = std::mem::take(&mut self.marks);
        self.marks_previous = Some(current);
        for path in previous {
            // Through the funnel, which is what leaves out the `..` row, and
            // only what is still there: an entry deleted in between does not
            // come back.
            if self.entries.iter().any(|e| e.path == path) {
                self.mark(path);
            }
        }
        Some(self.marks.len())
    }

    /// Clears every mark.
    pub fn clear_marks(&mut self) {
        self.snapshot_marks();
        self.marks.clear();
    }

    /// Marks (or unmarks) the visible entries with the SAME extension as the
    /// one under the cursor (#313). Returns how many marks it changed.
    ///
    /// The extension is the tail after the LAST dot of the base name, in
    /// bytes and without going through `String` (rule 1), and a leading dot
    /// does not open it: `.bashrc` has no extension, it has a name. With
    /// nothing under the cursor, or over something with no extension, it
    /// does nothing and returns 0 — marking "everything that also has no
    /// extension" is a different rule nobody asked for.
    ///
    /// The comparison is EXACT in bytes, not folded: `.TXT` and `.txt` are
    /// the same extension on Windows and two different ones on Linux, and
    /// the listing being looked at already knows which of the two it is —
    /// but that decision belongs to the volume and not to this function, so
    /// what is written on disk rules here.
    pub fn mark_same_extension(&mut self, mark: bool) -> usize {
        let Some(ext) = self.selected().and_then(|e| {
            e.path
                .file_name()
                .and_then(|n| extension_of(n.as_bytes()).map(<[u8]>::to_vec))
        }) else {
            return 0;
        };
        self.snapshot_marks();
        let mut changed = 0usize;
        for i in self.markable_indices() {
            let Some(entry) = self.entries.get(i) else {
                continue;
            };
            let its_ext = entry
                .path
                .file_name()
                .and_then(|n| extension_of(n.as_bytes()));
            if its_ext != Some(ext.as_slice()) {
                continue;
            }
            let path = entry.path.clone();
            let hit = if mark {
                self.mark(path)
            } else {
                self.marks.remove(&path)
            };
            if hit {
                changed += 1;
            }
        }
        changed
    }

    /// Marks the visible entries that are FILES (`dirs = false`) or the ones
    /// that are DIRECTORIES (`dirs = true`) (#313). Returns how many it
    /// added.
    ///
    /// ADDITIVE, like `mark.pattern-add`: it extends whatever was already
    /// marked instead of replacing it. A link counts as a file — that is
    /// what any operation of this panel does with it.
    pub fn mark_kind(&mut self, dirs: bool) -> usize {
        self.snapshot_marks();
        let mut changed = 0usize;
        for i in self.markable_indices() {
            let Some(entry) = self.entries.get(i) else {
                continue;
            };
            if (entry.kind == EntryKind::Dir) != dirs {
                continue;
            }
            let path = entry.path.clone();
            if self.mark(path) {
                changed += 1;
            }
        }
        changed
    }

    /// Marks again, BY PATH, whatever is still in the listing.
    ///
    /// Exists for the REFRESH: `set_listing` clears the marks because the
    /// rows are different ones and a mark by index would point at a
    /// different file. That is correct for a `cd`, and it punishes someone
    /// who did not move — a listing that reloads on its own (a copy that
    /// finishes, a watcher) used to sweep away a selection the reader had
    /// made by hand.
    ///
    /// Identity is the `VPath` BYTE FOR BYTE, as everywhere else: an entry
    /// that is no longer there —the operation just deleted it— simply does
    /// not get marked again, and nothing is invented. What it returns is how
    /// many were lost, because a selection that shrinks without saying so is
    /// a later operation over fewer files than the reader believes.
    ///
    /// ```
    /// use norte_frontend::PaneState;
    /// use norte_proto::{Entry, EntryKind, VPath};
    ///
    /// let dir = VPath::parse("mem:///d").unwrap();
    /// fn entry(dir: &VPath, n: &str) -> Entry {
    ///     Entry {
    ///         path: dir.join(norte_proto::Segment::new(n.as_bytes().to_vec()).unwrap()),
    ///         kind: EntryKind::File,
    ///         size: None,
    ///         mtime_ms: None,
    ///         attrs: Default::default(),
    ///     }
    /// }
    /// let mut p = PaneState::new(
    ///     dir.clone(),
    ///     vec![entry(&dir, "a"), entry(&dir, "b")],
    /// );
    /// p.mark_all();
    /// let before = p.marked_paths();
    /// assert_eq!(before.len(), 2);
    ///
    /// // The listing reloads and `b` is no longer there.
    /// p.set_listing(dir.clone(), vec![entry(&dir, "a")]);
    /// assert_eq!(p.marks_len(), 0, "a new listing arrives with no marks");
    /// assert_eq!(p.restore_marks(&before), 1, "one was lost, and it is said");
    /// assert_eq!(p.marks_len(), 1);
    /// ```
    pub fn restore_marks(&mut self, paths: &[VPath]) -> usize {
        let mut lost = 0;
        for path in paths {
            if self.entries.iter().any(|e| &e.path == path) {
                // Through the funnel: if what is being restored is the
                // PARENT (a mark inherited from when the `..` row could
                // sneak in as an entry), it falls here instead of
                // reappearing on the go-up row after every refresh. It does
                // not count as lost: it was never an entry.
                self.mark(path.clone());
            } else {
                lost += 1;
            }
        }
        lost
    }

    /// The indices a BULK mark acts on: the VISIBLE subset under an active
    /// quick filter, the whole listing otherwise — what you see is what you
    /// mark. While a fill is running ([`Self::loading`]) it reaches only what
    /// has been drained so far; the pane already marks an in-progress listing
    /// (the title in the TUI, a status line in the GUI), so the partial reach
    /// is never silent.
    /// The `..` row NEVER gets in: it is not an entry of this directory, and
    /// marking it would put the PARENT in the list of what gets copied or
    /// deleted. Here and not in every caller, because this is the choke
    /// point every bulk mark passes through.
    pub(super) fn markable_indices(&self) -> Vec<usize> {
        let from = usize::from(self.is_parent_row(0));
        match self.quick_visible() {
            Some(vis) => vis.iter().copied().filter(|i| *i >= from).collect(),
            None => (from..self.entries.len()).collect(),
        }
    }

    /// Marks every entry of the visible set (see `markable_indices`).
    pub fn mark_all(&mut self) {
        self.snapshot_marks();
        for i in self.markable_indices() {
            let Some(path) = self.entries.get(i).map(|e| &e.path) else {
                continue;
            };
            if !self.marks.contains(path) {
                let path = path.clone();
                self.mark(path);
            }
        }
    }

    /// Marks every entry between two indices of [`Self::entries`],
    /// INCLUSIVE, in either order (`mark_range(7, 2)` is `mark_range(2, 7)`)
    /// — the primitive a shift+click and a pointer sweep need, where the
    /// anchor sits either side of the pointer and the caller should not
    /// have to sort them first. Returns how many marks it ADDED, never the
    /// resulting total, exactly like [`Self::mark_glob`]: a range over
    /// already-marked entries returns 0 while the selection stays
    /// non-empty; read [`Self::marks_len`] for the total.
    ///
    /// It only ever ADDS — this is the ADDITIVE marker, the one a
    /// shift+click wants (it extends a selection built by hand and must not
    /// take anything back). A pointer SWEEP wants the opposite and uses
    /// [`Self::apply_sweep`], which rubber-bands against a baseline.
    /// [`Self::set_mark`] is the only way to clear a mark by index.
    ///
    /// Under an active [`Mode::Filter`] quick search it reaches only the
    /// VISIBLE subset (`markable_indices`, the same rule as
    /// [`Self::mark_all`]): what you cannot see, you cannot mark. A range
    /// whose ends straddle a filtered-out entry leaves that entry alone, so
    /// the next bulk operation never widens onto a file the filter was
    /// hiding.
    ///
    /// The range is CLAMPED to the listing, not rejected: a range that runs
    /// off the end marks up to the last entry, which is exactly what a hit
    /// test in the blank area below the last row produces. A range entirely
    /// outside the listing (and any range on an empty listing) therefore
    /// marks nothing.
    ///
    /// ```
    /// # use norte_frontend::PaneState;
    /// # use norte_proto::{Entry, EntryKind, VPath};
    /// # fn e(w: &str) -> Entry {
    /// #     Entry { attrs: Default::default(), path: VPath::parse(w).unwrap(),
    /// #             kind: EntryKind::File, size: None, mtime_ms: None }
    /// # }
    /// let mut p = PaneState::new(
    ///     VPath::parse("mem:///").unwrap(),
    ///     vec![e("mem:///a"), e("mem:///b"), e("mem:///c")],
    /// );
    /// assert_eq!(p.mark_range(2, 0), 3, "either order, inclusive");
    /// // The return is what CHANGED, never the resulting total: re-marking
    /// // the same range changes nothing while the selection stays full.
    /// assert_eq!(p.mark_range(0, 2), 0);
    /// assert_eq!(p.marks_len(), 3);
    /// ```
    pub fn mark_range(&mut self, from: usize, to: usize) -> usize {
        let (lo, hi) = if from <= to { (from, to) } else { (to, from) };
        // Walks the RANGE, not the whole listing: a sweep re-states its
        // range at pointer-event rate, and `markable_indices` costs a `Vec`
        // the size of the listing on every call.
        let Some(last) = self.entries.len().checked_sub(1) else {
            return 0;
        };
        if lo > last {
            return 0;
        }
        let hi = hi.min(last);
        let mut changed = 0usize;
        for i in lo..=hi {
            if !self.is_markable(i) {
                continue;
            }
            let Some(entry) = self.entries.get(i) else {
                continue;
            };
            // `contains` before cloning: a sweep's re-emission passes
            // through here at pointer-event rate and the vast majority of
            // the range's rows are already marked — cloning a `VPath` just
            // for the `HashSet` to throw it away was the dominant cost.
            if self.marks.contains(&entry.path) {
                continue;
            }
            let path = entry.path.clone();
            if self.mark(path) {
                changed += 1;
            }
        }
        changed
    }

    /// Arms a pointer sweep: drops any baseline left by a previous one, so
    /// the next [`Self::apply_sweep`] snapshots the marks as they are NOW.
    /// Cheap (no clone); call it when the gesture starts.
    ///
    /// Without this, a sweep that follows unrelated marking (a ctrl+click,
    /// a `mark_glob`) would restore the previous gesture's baseline and
    /// silently drop everything marked in between.
    pub fn begin_sweep(&mut self) {
        self.sweep_baseline = None;
        self.sweep_extent = None;
    }

    /// Applies the CURRENT extent of a pointer sweep: gives back the rows
    /// the sweep covered a moment ago and no longer does, then marks
    /// `from..=to` through [`Self::mark_range`], so the filter still decides
    /// what is reachable. Returns the marks added on top of what was already
    /// there.
    ///
    /// Both directions are DELTAS over the extent that changed, never a
    /// rebuild: rows that LEFT the range are given back (only those the
    /// sweep itself added — anything in the baseline is untouched) and only
    /// rows that ENTERED it are marked. A one-row motion therefore costs one
    /// row. This matters: at GPUI's per-pixel event rate on a 20 000-entry
    /// pane, restoring a snapshot per motion measured 7.1 ms per event and
    /// re-marking the whole range 2.7 ms, against 0.03 ms for the delta.
    ///
    /// The extent is dropped by any listing change, so the first call after
    /// one re-marks its whole range rather than trusting indices that moved.
    /// A quick filter that changes DURING a gesture is not re-examined: a
    /// row that becomes visible mid-sweep stays unmarked until the pointer
    /// moves over it again. Nothing is lost, and a drag with a hand on the
    /// filter is not a gesture worth a full rescan per motion.
    ///
    /// This is what makes a drag RUBBER-BAND. An add-only sweep leaves
    /// behind everything the pointer ever touched: overshooting by fifteen
    /// rows and pulling back leaves fifteen files marked, and because an
    /// overshoot happens at the viewport edge under autoscroll, those rows
    /// are precisely the ones that just scrolled out of sight. The next
    /// bulk operation would then act on files the user pulled back from and
    /// cannot see — and with no range unmarker, undoing that by hand is one
    /// ctrl+click per surplus row.
    ///
    /// The baseline is a snapshot of the mark SET, so marks made before the
    /// gesture survive every retreat. It is dropped by any listing change
    /// (see the `sweep_baseline` field); after one, the next call
    /// re-snapshots and the sweep simply starts rubber-banding from there.
    pub fn apply_sweep(&mut self, from: usize, to: usize) -> usize {
        let (lo, hi) = if from <= to { (from, to) } else { (to, from) };
        if self.sweep_baseline.is_none() {
            // Lazy snapshot: the gesture might have been armed by a plain
            // click that never sweeps, and cloning the mark set on every
            // click would be a cost nobody asked for.
            self.sweep_baseline = Some(self.marks.clone());
            self.sweep_extent = None;
        }
        if let (Some(baseline), Some((plo, phi))) = (&self.sweep_baseline, self.sweep_extent) {
            let phi = phi.min(self.entries.len().saturating_sub(1));
            let mut release: Vec<VPath> = Vec::new();
            for i in plo..=phi {
                if i >= lo && i <= hi {
                    continue;
                }
                let Some(entry) = self.entries.get(i) else {
                    continue;
                };
                // Only what THIS sweep put down gets released: whatever was
                // already marked before the gesture is in the baseline and
                // is not touched.
                if !baseline.contains(&entry.path) {
                    release.push(entry.path.clone());
                }
            }
            for path in release {
                self.marks.remove(&path);
            }
        }
        // Marks only what ENTERS the range: the rest of the overlap was
        // already marked by an earlier call of this same sweep. Two
        // intervals at most, so a one-row motion costs one row and not a
        // pass over the whole listing.
        let previous = self.sweep_extent;
        self.sweep_extent = Some((lo, hi));
        match previous {
            Some((plo, phi)) if lo <= phi && hi >= plo => {
                let mut changed = 0usize;
                if lo < plo {
                    changed += self.mark_range(lo, plo - 1);
                }
                if hi > phi {
                    changed += self.mark_range(phi + 1, hi);
                }
                changed
            }
            _ => self.mark_range(lo, hi),
        }
    }

    /// Gives back EVERYTHING the sweep in progress marked, restoring the
    /// baseline [`Self::apply_sweep`] snapshotted, and keeps the gesture
    /// armed: a later `apply_sweep` starts rubber-banding from the same
    /// baseline, so a pointer that leaves and comes back loses nothing.
    ///
    /// It is [`Self::apply_sweep`] with an EMPTY extent, and it exists for
    /// the moment a mark sweep stops being one: a drag that crosses into the
    /// other pane is promoted to a transfer
    /// ([`crate::mouse::Effect::RevertSweep`]), and a promotion changes what
    /// the gesture DOES, not what is selected — the rows it swept on the way
    /// out must not stay marked behind it.
    ///
    /// Marks made BEFORE the gesture survive (they are in the baseline),
    /// exactly as they survive a retreat. Without an armed sweep it is a
    /// no-op.
    ///
    /// ```
    /// # use norte_frontend::PaneState;
    /// # use norte_proto::{Entry, EntryKind, VPath};
    /// # fn e(w: &str) -> Entry {
    /// #     Entry { attrs: Default::default(), path: VPath::parse(w).unwrap(),
    /// #             kind: EntryKind::File, size: None, mtime_ms: None }
    /// # }
    /// let mut p = PaneState::new(
    ///     VPath::parse("mem:///").unwrap(),
    ///     vec![e("mem:///a"), e("mem:///b"), e("mem:///c")],
    /// );
    /// p.set_mark(2, true); // mark from before the gesture
    /// p.begin_sweep();
    /// p.apply_sweep(0, 1);
    /// assert_eq!(p.marks_len(), 3);
    /// p.revert_sweep();
    /// assert_eq!(p.marks_len(), 1, "only the earlier mark survives");
    /// ```
    pub fn revert_sweep(&mut self) {
        let extent = self.sweep_extent.take();
        let mut release: Vec<VPath> = Vec::new();
        if let (Some(baseline), Some((lo, hi))) = (self.sweep_baseline.as_ref(), extent) {
            let hi = hi.min(self.entries.len().saturating_sub(1));
            for i in lo..=hi {
                let Some(entry) = self.entries.get(i) else {
                    continue;
                };
                // Only what THIS sweep put down gets released: whatever was
                // there before the gesture is in the baseline and is not
                // touched.
                if !baseline.contains(&entry.path) {
                    release.push(entry.path.clone());
                }
            }
        }
        for path in release {
            self.marks.remove(&path);
        }
    }

    /// Ends a pointer sweep, releasing its baseline. Idempotent, and not
    /// required for correctness ([`Self::begin_sweep`] re-arms anyway) —
    /// it only stops a mark-set-sized snapshot from outliving the gesture.
    pub fn end_sweep(&mut self) {
        self.sweep_baseline = None;
        self.sweep_extent = None;
    }

    /// Is this index reachable by a mark right now? The VISIBLE subset
    /// under an active [`Mode::Filter`] quick search, any listed index
    /// otherwise — `markable_indices` without materialising it.
    pub(super) fn is_markable(&self, index: usize) -> bool {
        // The `..` row is never marked: marking it would put the PARENT in
        // what gets copied or deleted. Same rule as `markable_indices`, and
        // it is in both because they are the two choke points marking goes
        // through — one for bulk operations, one for a single row.
        if self.is_parent_row(index) {
            return false;
        }
        match self.quick_visible() {
            // `vis` comes in ASCENDING order (`nav::matches_folded`
            // enumerates `entries` in order and filters), an invariant
            // pinned by `quick_visible_comes_in_ascending_order`: the binary
            // search avoids a linear scan for every row of the range.
            Some(vis) => vis.binary_search(&index).is_ok(),
            None => index < self.entries.len(),
        }
    }

    /// Marks (`marked = true`) or unmarks (`false`) ONE entry by its index
    /// in [`Self::entries`] — the primitive a ctrl+click needs, naming a row
    /// directly instead of the cursor ([`Self::toggle_mark`], which only
    /// ever reaches the selection). No-op if the index is outside the
    /// listing.
    ///
    /// MARKING respects the quick filter and UNMARKING does not, and the
    /// asymmetry is deliberate. An index is resolved from a painted frame
    /// against a listing that is not index-stable — an incremental fill
    /// inserts entries, a `refill` prunes them, a re-sort moves them — so by
    /// the time the index arrives here it can name a different entry than
    /// the one under the pointer, possibly one the filter hides. Marking
    /// the wrong entry WIDENS the next bulk operation onto a file nobody
    /// chose; unmarking the wrong entry only ever shrinks it. Only the
    /// first of those can destroy data, so only the first is refused.
    ///
    /// Unmarking down to an empty set re-arms [`Self::marked_paths`]'s
    /// cursor fallback, the same caveat [`Self::mark_glob`] carries.
    pub fn set_mark(&mut self, index: usize, marked: bool) {
        if marked && !self.is_markable(index) {
            return;
        }
        let Some(path) = self.entries.get(index).map(|e| e.path.clone()) else {
            return;
        };
        if marked {
            self.mark(path);
        } else {
            self.marks.remove(&path);
        }
    }

    /// Flips the mark of every entry of the visible set (see
    /// `markable_indices`). Marks OUTSIDE that set SURVIVE untouched:
    /// invert is "flip what you see", not "replace the selection with its
    /// complement" — under a filter, [`Self::marked_paths`] can therefore
    /// still return entries the user is not looking at.
    pub fn invert_marks(&mut self) {
        self.snapshot_marks();
        for i in self.markable_indices() {
            let Some(path) = self.entries.get(i).map(|e| e.path.clone()) else {
                continue;
            };
            if !self.marks.remove(&path) {
                self.mark(path);
            }
        }
    }

    /// Marks (`mark = true`) or unmarks (`false`) the visible entries whose
    /// name matches `pattern`, a glob. Returns how many marks it ADDED or
    /// REMOVED, never the resulting total — a pattern that only re-marks
    /// what was already marked returns 0 even though the selection is
    /// non-empty; read [`Self::marks_len`] for the total.
    ///
    /// Matching folds BOTH sides through the quick-search pipeline
    /// ([`nav::fold_with`](crate::nav::fold_with): lossy UTF-8 → NFC →
    /// lowercase → NFC, honouring the pane's name reinterpretation) before
    /// compiling the glob, so a pattern matches the FOLDED name, not the
    /// text the pane paints: [`crate::display_name_with`] additionally
    /// MASKS bidi overrides and invisibles to U+FFFD, which the fold does
    /// not — a name typed exactly as painted only matches if it is already
    /// NFC, lowercase, and free of masked characters. Folding the pattern
    /// is what makes NFD and uppercase input match: the fold is the ONE
    /// definition of name equality, shared with the quick search. The glob
    /// deliberately does NOT add `case_insensitive` on top — regex-crate
    /// case folding is wider than the fold (`s` would match `ſ` U+017F)
    /// and would mark files the quick search considers distinct.
    ///
    /// A non-UTF-8 name's invalid bytes fold to U+FFFD and cannot be named
    /// INDIVIDUALLY — but typing U+FFFD in the pattern names ALL of them at
    /// once, matching every hostile name whose lossy form collapses there.
    /// [`Self::toggle_mark`] always reaches an entry by hand regardless, and
    /// [`Self::marked_paths`] returns each mark's original bytes untouched
    /// (hard rule 1).
    ///
    /// `?` and a character class (`[...]`) count CHARACTERS (#110): the
    /// glob's byte-mode regex is recompiled in Unicode mode
    /// (`unicode_glob_regex`), so `a?o` matches `año` even though `ñ` is
    /// two bytes. Unmarking down to an empty set re-arms
    /// [`Self::marked_paths`]'s cursor fallback (it returns the entry under
    /// the cursor when no marks remain) — a caller must read the count this
    /// method returns rather than assume the mark set still reflects what
    /// the user last saw.
    ///
    /// # Errors
    /// [`PatternError::Glob`] if the pattern does not compile. Nothing is
    /// marked in that case.
    pub fn mark_glob(&mut self, pattern: &str, mark: bool) -> Result<usize, PatternError> {
        self.snapshot_marks();
        // The pattern is folded with the SAME pipeline as the name (#103):
        // the fold is Unicode, globset's `case_insensitive` is ASCII-only
        // (it emits `(?-u)`), so without folding the needle an NFD pattern
        // or a non-ASCII uppercase letter would match NOTHING, silently.
        let folded = crate::nav::fold(pattern.as_bytes());
        // WITHOUT `case_insensitive`: the fold already lowercases BOTH
        // sides, and the Unicode regex's `(?i)` is a WIDER case-folding than
        // the fold (`s` would match `ſ` U+017F, `μ` would match `µ` U+00B5)
        // — it would mark files the quick search considers distinct. ONE
        // single definition of equality: the fold's (audit #110).
        let glob = GlobBuilder::new(&folded)
            .backslash_escape(true) // otherwise `\`'s semantics depend on the
            // OS (globset makes it depend on `is_separator('\\')`, true on
            // unix, false on windows) — `\` is a legal name byte on Linux
            // (corpus `win_backslash`) and the pattern must match it the
            // same way on both.
            .build()
            .map_err(|e| PatternError::Glob(e.to_string()))?;
        // Unicode mode (#110): `?`/classes count CHARACTERS, not bytes.
        // `size_limit` because this is a public API with no cap of its own
        // (the TUI's modal bounds it to 256 chars, but nothing forces other
        // callers to); the `regex` engine is linear, so the guard is about
        // the compiled program's memory, not backtracking.
        // `dot_matches_new_line`: globset compiles its matcher with that
        // flag and `*`/`?` translate to `.`-derived expressions — without
        // it, a name with `\n` (a legal byte on unix, corpus
        // `control_newline`) would stop matching `*`, SILENTLY.
        let matcher = regex::RegexBuilder::new(&unicode_glob_regex(&glob)?)
            .size_limit(1 << 20)
            .dot_matches_new_line(true)
            .build()
            .map_err(|e| PatternError::Glob(e.to_string()))?;
        let enc = self.name_encoding;
        let mut changed = 0usize;
        for i in self.markable_indices() {
            let Some(entry) = self.entries.get(i) else {
                continue;
            };
            let name = entry.path.file_name().map_or(&b""[..], |n| n.as_bytes());
            if !matcher.is_match(crate::nav::fold_with(name, enc).as_str()) {
                continue;
            }
            let path = entry.path.clone();
            let hit = if mark {
                self.mark(path)
            } else {
                self.marks.remove(&path)
            };
            if hit {
                changed += 1;
            }
        }
        Ok(changed)
    }

    /// Total size of every marked entry that is NOT a directory, saturating.
    /// A symlink contributes its own size, never its target's. Directories
    /// contribute 0: nothing here walks a tree, and a status bar that added
    /// a directory's own inode size would be claiming a total it never
    /// computed.
    #[must_use]
    pub fn marked_bytes(&self) -> u64 {
        // With no marks there is nothing to add up: walking twenty thousand
        // entries summing every path, on every keystroke, was half the cost
        // of moving the cursor in a large directory.
        if self.marks.is_empty() {
            return 0;
        }
        self.entries
            .iter()
            .filter(|e| e.kind != EntryKind::Dir && self.marks.contains(&e.path))
            .fold(0u64, |acc, e| acc.saturating_add(e.size.unwrap_or(0)))
    }

    /// How many marked entries are directories. [`Self::marked_bytes`]
    /// deliberately excludes directories (nothing here walks a tree), so a
    /// status bar that renders `marked_bytes` alone would understate a
    /// selection that includes one: a marked 10-byte file plus a 40 GiB
    /// directory must not read as "2 marked, 10 B" — that reads like a
    /// transfer size and is not one. Callers name the directory count
    /// separately instead of folding it into a total nobody computed.
    #[must_use]
    pub fn marked_dirs(&self) -> usize {
        if self.marks.is_empty() {
            return 0;
        }
        self.entries
            .iter()
            .filter(|e| e.kind == EntryKind::Dir && self.marks.contains(&e.path))
            .count()
    }

    /// Marks dropped by the last [`Self::refill`] because their entry was
    /// gone (#103) — see the `pruned_marks` field. Zero after a `cd`
    /// ([`Self::set_listing`]/[`Self::begin_loading`]) or when nothing was
    /// pruned. A later task surfaces this in the status bar; this accessor
    /// alone adds no UI.
    #[must_use]
    pub fn pruned_marks(&self) -> usize {
        self.pruned_marks
    }

    /// Drops marks whose entry is no longer listed and returns how many were
    /// dropped. A mark is a claim about an entry that EXISTS: a stale path
    /// would silently widen the next bulk operation. Called from
    /// [`Self::refill`], the same-dir refresh: the only path that can drop an
    /// entry without a `cd`. A paginated fill ([`Self::extend`], ADR 0017)
    /// only ADDS entries, so a mark placed mid-fill always points at
    /// something present and needs no pruning there.
    ///
    /// Accepted TOCTOU: identity here is the byte-exact `VPath` alone (hard
    /// rule 1) — the entry's `kind` is not part of it. If an external actor
    /// deletes a marked file and recreates a directory at the same path
    /// between listings, the mark survives the prune and a bulk operation
    /// acts on whatever now lives at that path, file or directory.
    pub(super) fn prune_marks(&mut self) -> usize {
        if self.marks.is_empty() {
            return 0;
        }
        let before = self.marks.len();
        let present: HashSet<&VPath> = self.entries.iter().map(|e| &e.path).collect();
        self.marks.retain(|p| present.contains(p));
        before - self.marks.len()
    }
}
