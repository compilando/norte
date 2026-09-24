//! A pane: its entries, its cursor, its marks and the search dialog that
//! lives inside it.

use norte_proto::{Entry, VPath};

/// A panel: current directory and its ALREADY sorted entries.
///
/// A pane's PURE mechanics — directory, entries, cursor, `loading` and quick
/// search — live ONCE in [`norte_frontend::PaneState`], shared with the GUI
/// (#82): the TUI's `Pane` EMBEDS it in its (private) `state` field and
/// delegates to it (`dir`/`entries`/`cursor`/`selected`/`move_*`/`quick_*`…).
/// What's specific to the TUI — the live search (`virtual_search`,
/// `search_state`, `search_error`) and the paginated fill (ADR 0017,
/// [`Pane::extend_listing`] and friends) — stays here, on top of that state.
#[derive(Debug)]
pub struct Pane {
    /// Shared non-render state (cursor + quick + listing). Private: accessed
    /// through the delegates ([`Pane::dir`], [`Pane::entries`], …) so
    /// `PaneState` guards the cursor's invariant.
    state: norte_frontend::PaneState,
    /// The pane shows the HITS of a live search (`Alt+F7`, liveSearch), not
    /// a real directory listing: `dir` is the walk's ROOT and `entries` are
    /// the results arriving by streaming ([`Pane::extend_listing`], reusing
    /// the pagination mold). With this on, the status bar paints
    /// `search-status-*` instead of the normal `pos/total`; any normal
    /// `cd`/refresh turns it off (real listings set it to `false`).
    /// `F5`/`F8`/`F3` operate ONLY on the hit under the cursor
    /// ([`Pane::selected`] gives the `Entry` with its full `VPath`).
    pub virtual_search: bool,
    /// A live search's presentation state (only meaningful with
    /// [`Pane::virtual_search`]): decides which `search-status-*` variant
    /// the status bar paints. The run loop updates it on the terminal state
    /// arriving.
    pub search_state: SearchState,
    /// Category of a search that FAILED (`SearchState::Failed`), already
    /// localized and sanitized: the status bar paints it PERSISTENTLY
    /// (`search-status-failed`) after [`crate::app::App::message`] clears —
    /// a failure can't degrade into "done" on the next key (review
    /// MINOR-2).
    pub search_error: Option<String>,
    /// Content match context per live-search hit (#81):
    /// `path → (line, preview ALREADY sanitized at the source)`. Only
    /// meaningful with [`Pane::virtual_search`]; the status bar paints it
    /// for the hit under the cursor. Cleared on leaving virtual mode
    /// (cd/real listing).
    pub search_matches: std::collections::HashMap<VPath, norte_proto::methods::MatchInfo>,
    /// This pane's listing could NOT be done on restoring the session, and
    /// what's shown isn't "this directory is empty" (#235).
    ///
    /// Marked in the pane's TITLE, same as pagination in progress, and not
    /// in a `message`: it's a state that lasts until someone actually
    /// lists, and a message gets erased by the next key — which is exactly
    /// the bug #232 fixes two hunks up. Any real listing
    /// ([`Pane::set_listing`], [`Pane::begin_listing`]) turns it off.
    pub unlisted: bool,
    /// The USER's hidden-files preference (#107): the virtual search pane
    /// SUSPENDS the filter (a hit is an EXPLICIT request — a searched-for
    /// `.env` silently disappearing under `[ui] show_hidden = false` was the
    /// review's MAJOR-1), and this gets restored on returning to a real
    /// listing. A Ctrl+H INSIDE the virtual pane acts on the results but
    /// doesn't touch the preference.
    show_hidden_pref: bool,
}

/// A live search's presentation state (`Alt+F7`, liveSearch T6): the run
/// loop reflects it in [`Pane::search_state`] so the status bar picks the
/// `search-status-*` variant. `Failed` isn't painted on the pane's status
/// bar (the concrete error travels via [`crate::app::App::message`] through
/// `error_message`); the variant is kept for completeness of the run's
/// state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SearchState {
    /// The walker keeps emitting hits.
    #[default]
    Running,
    /// Finished and the hit cap wasn't reached.
    Done,
    /// Finished by reaching `max_hits` (results possibly incomplete).
    Truncated,
    /// The user cancelled (hits already received are kept).
    Cancelled,
    /// The search Task failed (the error travels via the message bar).
    Failed,
}

impl Pane {
    /// Pane over `dir` with `entries`: #54, no longer needs sorting them
    /// beforehand — [`norte_frontend::PaneState::new`] normalizes
    /// internally (dirs first, NFC, ties by bytes, see
    /// [`crate::app::sort_entries`]).
    #[must_use]
    pub fn new(dir: VPath, entries: Vec<Entry>) -> Self {
        Self {
            state: norte_frontend::PaneState::new(dir, entries),
            virtual_search: false,
            search_state: SearchState::Running,
            search_error: None,
            search_matches: std::collections::HashMap::new(),
            show_hidden_pref: true,
            unlisted: false,
        }
    }

    /// Cycles the name reinterpretation (#57): pure delegate — the
    /// mechanics (suggestion, full cycle wrap, re-folding a live quick
    /// search) live in
    /// [`norte_frontend::PaneState::cycle_name_encoding`] (#98/m2: the GUI
    /// reuses it as is).
    pub fn cycle_name_encoding(&mut self) -> Option<&'static str> {
        self.state.cycle_name_encoding()
    }

    /// Active name reinterpretation (#57), for rendering.
    #[must_use]
    pub fn name_encoding(&self) -> Option<norte_encoding::NameEncoding> {
        self.state.name_encoding()
    }

    /// The shared state, for what `norte-frontend` decides over it WHOLE
    /// (the status bar, ADR 0132) instead of field by field.
    #[must_use]
    pub(crate) fn state(&self) -> &norte_frontend::PaneState {
        &self.state
    }

    // --- Read-only delegates over the shared state (#82) ---

    /// Listed directory.
    #[must_use]
    pub fn dir(&self) -> &VPath {
        self.state.dir()
    }

    /// Sorted entries ([`crate::app::sort_entries`]), the `..` row included:
    /// it's the list that gets PAINTED.
    #[must_use]
    pub fn entries(&self) -> &[Entry] {
        self.state.entries()
    }

    /// The REAL entries, without the `..` row: what gets copied when a pane
    /// is born from another one's listing
    /// ([`crate::app::App::fork_pane`]).
    #[must_use]
    pub fn real_entries(&self) -> &[Entry] {
        self.state.real_entries()
    }

    /// The files an ORGANIZE plan can move (phase 8), as text
    /// ([`norte_frontend::PaneState::organizable_names`]).
    #[must_use]
    pub fn organizable_names(&self) -> Vec<String> {
        self.state.organizable_names()
    }

    /// The names already occupying this directory, to tell a new folder
    /// from one that already existed
    /// ([`norte_frontend::PaneState::existing_names`]).
    #[must_use]
    pub fn existing_names(&self) -> Vec<String> {
        self.state.existing_names()
    }

    /// What the cursor points at for a panel gesture: the folder under it if
    /// it is one, and if not this directory
    /// ([`norte_frontend::PaneState::target_dir`]).
    #[must_use]
    pub fn target_dir(&self) -> &VPath {
        self.state.target_dir()
    }

    /// Index under the real cursor (0 even with an empty list).
    #[must_use]
    pub fn cursor(&self) -> usize {
        self.state.cursor()
    }

    /// The listing is FILLING IN in the background (pagination, ADR 0017):
    /// the first page already painted and more entries are arriving. The UI
    /// marks it — an incomplete listing is NEVER silent.
    #[must_use]
    pub fn loading(&self) -> bool {
        self.state.loading()
    }

    /// Live quick search (`/`, spec 2026-07-18) for rendering; `None` =
    /// normal navigation.
    #[must_use]
    pub fn quick(&self) -> Option<&crate::nav::QuickSearch> {
        self.state.quick()
    }

    /// The selected entry: with quick search in Filter mode, the selection
    /// WITHIN the filter (so F5/F8/F3… operate on what's filtered without
    /// each command knowing about quick search — feed-to-listbox); if the
    /// filter has no matches, `None` (ops no-op, never acting on an entry
    /// the user doesn't see). With no filter (or in Jump, which moves the
    /// real cursor), the entry under the cursor.
    #[must_use]
    pub fn selected(&self) -> Option<&Entry> {
        self.state.selected()
    }

    /// The entry under the cursor TO DESCRIBE IT, `..` row included.
    ///
    /// The other question, the one panels that follow the cursor ask: see
    /// [`norte_frontend::PaneState::cursor_entry`]. **It isn't an operand.**
    #[must_use]
    pub fn cursor_entry(&self) -> Option<&Entry> {
        self.state.cursor_entry()
    }

    /// Is what's pointed at RIGHT NOW the `..` row? See
    /// [`norte_frontend::PaneState::cursor_is_parent_row`] — it comes from
    /// the same index as [`Self::cursor_entry`], which is why it isn't
    /// `is_parent_row(cursor())`.
    #[must_use]
    pub fn cursor_is_parent_row(&self) -> bool {
        self.state.cursor_is_parent_row()
    }

    /// REAL indices visible under the filter; `None` = no filter (quick
    /// inactive, or Jump mode: the whole listing gets painted).
    #[must_use]
    pub fn quick_visible(&self) -> Option<&[usize]> {
        self.state.quick_visible()
    }

    /// POINTS AT entry `i`: the cursor, or the filter's selection when a
    /// live quick search is on. See [`norte_frontend::PaneState::senalar`]
    /// — with a filter, `set_cursor` moves something nobody is looking at.
    pub fn senalar(&mut self, i: usize) {
        self.state.senalar(i);
    }

    // --- Cursor + quick mutation delegates (#82) ---

    /// Starts quick search (`/`) in `mode` over the current entries.
    pub fn quick_start(&mut self, mode: crate::nav::Mode) {
        self.state.quick_start(mode);
    }

    /// A character typed with quick search active.
    pub fn quick_char(&mut self, c: char) {
        self.state.quick_char(c);
    }

    /// Backspace with quick search active.
    pub fn quick_backspace(&mut self) {
        self.state.quick_backspace();
    }

    /// Quick search selection one position down.
    pub fn quick_down(&mut self) {
        self.state.quick_down();
    }

    /// Quick search selection one position up.
    pub fn quick_up(&mut self) {
        self.state.quick_up();
    }

    /// Next match with wrap (Tab in Jump mode).
    pub fn quick_next(&mut self) {
        self.state.quick_next();
    }

    /// Closes quick search WITHOUT touching the real cursor: in Filter the
    /// full listing comes back with the cursor where it was (the filter
    /// never moved it — a test from the plan); in Jump the cursor stays
    /// where it jumped to.
    pub fn quick_cancel(&mut self) {
        self.state.quick_cancel();
    }

    /// Closes quick search, setting the REAL cursor to the selection
    /// (Enter: the next op starts from there). Returns `true` if the cursor
    /// points at an entry the user COULD SEE: in Filter with no matches it
    /// returns `false` (the painted list was empty — never dispatch over an
    /// invisible entry); in Jump with no matches it returns `true` if there
    /// are entries (the listing is painted WHOLE: the real cursor is
    /// visible by definition — a T4 edge case, reviewer's observation).
    pub fn quick_confirm(&mut self) -> bool {
        self.state.quick_confirm()
    }

    /// Moves the cursor up `n` positions (clamped at 0).
    pub fn move_up(&mut self, n: usize) {
        self.state.page_up(n);
    }

    /// Moves the cursor down `n` positions (clamped at the last entry).
    pub fn move_down(&mut self, n: usize) {
        self.state.page_down(n);
    }

    /// Listing rows painted in the last frame (#124) — pure delegate to
    /// [`norte_frontend::PaneState::set_viewport_rows`].
    pub fn set_viewport_rows(&mut self, rows: usize) {
        self.state.set_viewport_rows(rows);
    }

    /// Leaves the window ready to paint `rows` rows — pure delegate to
    /// [`norte_frontend::PaneState::reconcile_viewport`]. The run loop calls
    /// it BEFORE every draw.
    pub fn reconcile_viewport(&mut self, rows: usize) {
        self.state.reconcile_viewport(rows);
    }

    /// The listing's first visible row — pure delegate to
    /// [`norte_frontend::PaneState::viewport_offset`]. Read by both the
    /// painter and the mouse's hit test, which have to see the SAME window.
    #[must_use]
    pub fn viewport_offset(&self) -> usize {
        self.state.viewport_offset()
    }

    /// How many rows a page moves in this pane (#124) — pure delegate to
    /// [`norte_frontend::PaneState::page_step`].
    #[must_use]
    pub fn page_step(&self) -> usize {
        self.state.page_step()
    }

    /// Cursor to the first entry.
    pub fn move_to_start(&mut self) {
        self.state.home();
    }

    /// Cursor to the last entry.
    pub fn move_to_end(&mut self) {
        self.state.end();
    }

    /// Sets the REAL cursor to `i` (clamped at the last entry): re-anchoring
    /// after locating a concrete index, e.g. a search hit.
    pub fn set_cursor(&mut self, i: usize) {
        self.state.set_cursor(i);
    }

    /// Pending focus (spec 2026-07-24 §S1, `nav.parent`): the next
    /// [`Pane::begin_listing`] selects `child` if it shows up in the new
    /// listing, ahead of the cursor memory. See
    /// [`norte_frontend::PaneState::set_pending_focus`].
    pub fn set_pending_focus(&mut self, child: VPath) {
        self.state.set_pending_focus(child);
    }

    /// Discards a pending focus without consuming it (S review, M2). See
    /// [`norte_frontend::PaneState::clear_pending_focus`].
    pub fn clear_pending_focus(&mut self) {
        self.state.clear_pending_focus();
    }

    // --- Listing + live search (specific to the TUI, on top of the state) ---

    /// Sets (or clears) a paginated fill's loading flag (ADR 0017).
    pub fn set_loading(&mut self, loading: bool) {
        self.state.set_loading(loading);
    }

    /// Starts a live search's virtual pane (`Alt+F7`, liveSearch T6): `root`
    /// is the walk's root, entries start empty and hits come in through
    /// [`Pane::extend_listing`] like a paginated listing. Marks the pane as
    /// virtual (the status bar paints `search-status-running`) and kills any
    /// live quick search (it was filtering something ELSE).
    pub fn begin_search(&mut self, root: VPath) {
        // #107: hits are EXPLICIT — the hidden-files filter is suspended in
        // the virtual pane (the preference stays in `show_hidden_pref`).
        self.state.set_show_hidden(true);
        self.state.set_listing(root, Vec::new());
        self.virtual_search = true;
        self.search_state = SearchState::Running;
        self.search_error = None;
        // #81 (review MAJOR-4): relaunching Alt+F7 with no cd in between
        // must not drag along the PREVIOUS search's previews (a hit from
        // name-only query B would paint query A's :line) nor grow the map
        // without bound between searches.
        self.search_matches.clear();
    }

    /// Replaces the content after a cd/refresh, resetting the cursor. A
    /// live quick search dies: it was filtering a DIFFERENT listing.
    pub fn set_listing(&mut self, dir: VPath, entries: Vec<Entry>) {
        // #107: on returning to a real listing, the user's hidden-files
        // preference takes over again BEFORE ingesting (the filter applies
        // as the listing comes in).
        self.state.set_show_hidden(self.show_hidden_pref);
        self.state.set_listing(dir, entries);
        self.virtual_search = false;
        self.search_matches.clear();
        self.state.set_skipped(None);
        self.unlisted = false;
    }

    /// A paginated listing's first page: replaces the content and MARKS
    /// that entries are still to arrive (ADR 0017). The drainer will keep
    /// calling [`Pane::extend_listing`] and, on finishing,
    /// [`Pane::finish_listing`]. `skipped` = the ones the container omitted
    /// (#93), from the listing's open.
    ///
    /// The cursor memory's capture point (spec §S1) for the TUI: unlike the
    /// GUI (which has an optimistic `begin_loading` phase BEFORE the async
    /// fetch), the TUI waits for the WHOLE listing before touching the pane
    /// (`cd` in `main.rs` doesn't call
    /// [`norte_frontend::PaneState::begin_loading`] — this method is the
    /// only point where `self.state` still reflects the OLD dir). Recording
    /// here, before `set_listing`, is the exact equivalent.
    pub fn begin_listing(
        &mut self,
        dir: VPath,
        first_page: Vec<Entry>,
        more: bool,
        skipped: Option<u64>,
    ) {
        self.state.remember_cursor();
        // #107: the same reset as `set_listing` — this is the TUI's real
        // paginated cd.
        self.state.set_show_hidden(self.show_hidden_pref);
        self.state.set_listing(dir, first_page);
        self.state.set_loading(more);
        self.virtual_search = false;
        self.search_matches.clear();
        self.state.set_skipped(skipped);
        self.unlisted = false;
    }

    /// Adds a batch from the drainer: re-sorts the WHOLE listing and
    /// re-anchors the cursor to the path that was selected (if it dropped
    /// out of the re-sort, clamp by index) so that filling doesn't move the
    /// user's selection under their feet. A live quick search gets
    /// RE-APPLIED over the new listing (spec: the filter doesn't freeze
    /// while the fill continues), keeping its selection by path. The pure
    /// mechanics live in [`norte_frontend::PaneState::extend`].
    pub fn extend_listing(&mut self, batch: Vec<Entry>) {
        self.state.extend(batch);
    }

    /// Hydrates size/mtime of entry `path` with a stat-on-focus probe's
    /// result (#52, lazy listing). Doesn't re-sort; no-op if the entry is no
    /// longer there. Pure delegate to
    /// [`norte_frontend::PaneState::hydrate`].
    pub fn hydrate(&mut self, path: &VPath, size: Option<u64>, mtime_ms: Option<i64>) {
        self.state.hydrate(path, size, mtime_ms);
    }

    /// VISIBLE paths with no `size` within `radius` rows of the cursor
    /// (#52) — pure delegate to
    /// [`norte_frontend::PaneState::needs_stat_window`].
    #[must_use]
    pub fn needs_stat_window(&self, radius: usize) -> Vec<VPath> {
        self.state.needs_stat_window(radius)
    }

    /// The drainer finished: the listing is now complete. Quick search gets
    /// re-applied by contract (today it doesn't mutate entries: a cheap
    /// refresh; if closing ever re-sorts, the filter won't be left holding
    /// dead indices).
    pub fn finish_listing(&mut self) {
        self.state.set_loading(false);
        self.state.refresh_quick();
    }

    /// A brand-new COMPLETE listing of the SAME dir (refresh after a
    /// mutation): cursor kept by INDEX with a clamp (after a delete it lands
    /// on the next entry — orthodox semantics) and quick search re-applied
    /// by path (the old listing's indices don't identify anything).
    pub fn refresh_listing(&mut self, entries: Vec<Entry>) {
        self.state.refill(entries);
        self.state.set_loading(false);
        self.virtual_search = false;
        self.search_matches.clear();
    }

    /// Omitted by the current listing's container (#93/#96) — pure delegate
    /// to [`norte_frontend::PaneState::skipped`]. The status bar paints
    /// `Some(n)`, n>0.
    #[must_use]
    pub fn skipped(&self) -> Option<u64> {
        self.state.skipped()
    }

    /// Sets the fresh omitted count (#96) — see `PaneState::set_skipped`.
    pub fn set_skipped(&mut self, skipped: Option<u64>) {
        self.state.set_skipped(skipped);
    }

    /// `path`'s plugin decoration (G3b, ADR 0037) — pure delegate to
    /// [`norte_frontend::PaneState::decoration_for`]. Rendering paints it as
    /// a badge after the hostile badge's slot.
    #[must_use]
    pub fn decoration_for(&self, path: &VPath) -> Option<&norte_frontend::Decoration> {
        self.state.decoration_for(path)
    }

    /// Whether any entry has an icon (ADR 0105): if so, rendering opens the
    /// icon column on every row. Pure delegate to
    /// [`norte_frontend::PaneState::any_icon`].
    #[must_use]
    pub fn any_icon(&self) -> bool {
        self.state.any_icon()
    }

    /// Cells covering 80% of the listing's names — pure delegate to
    /// [`norte_frontend::PaneState::name_width_p80`].
    #[must_use]
    pub fn name_width_p80(&self) -> u16 {
        self.state.name_width_p80()
    }

    /// Is this entry marked? (#103) — pure delegate to
    /// [`norte_frontend::PaneState::is_marked`]. Rendering paints a textual
    /// gutter (`*`) at the start of the row.
    #[must_use]
    pub fn is_marked(&self, entry: &Entry) -> bool {
        self.state.is_marked(entry)
    }

    /// A `plugin:` column's cell (#117-follow-up) — pure delegate to
    /// [`norte_frontend::PaneState::plugin_cell`].
    #[must_use]
    pub fn plugin_cell(&self, display_id: &str, path: &VPath) -> Option<String> {
        self.state.plugin_cell(display_id, path)
    }

    /// Installs the batch of `plugin:` column values (#117-follow-up) —
    /// pure delegate to [`norte_frontend::PaneState::set_plugin_columns`].
    pub fn set_plugin_columns(
        &mut self,
        columns: std::collections::HashMap<String, std::collections::HashMap<VPath, String>>,
    ) {
        self.state.set_plugin_columns(columns);
    }

    /// Toggles the selected entry's mark. Pure delegate (#103).
    pub fn toggle_mark(&mut self) {
        self.state.toggle_mark();
    }

    /// mc/Total Commander: toggles the VISIBLE selection's mark and advances
    /// (within the filter if one is active, otherwise the real cursor; no
    /// wrapping on the last row). Pure delegate to
    /// [`norte_frontend::PaneState::toggle_mark_and_advance`] (#103, review:
    /// the "what it advances over" mechanics can't be reimplemented here
    /// nor in dispatch — it lives once in the shared model).
    pub fn toggle_mark_and_advance(&mut self) {
        self.state.toggle_mark_and_advance();
    }

    /// The mirror of the one above, UPWARD (`shift+↑`). Pure delegate.
    pub fn toggle_mark_and_retreat(&mut self) {
        self.state.toggle_mark_and_retreat();
    }

    /// Marks (or unmarks) the `n`-row stretch from the cursor and moves
    /// there (`shift+PgDn`/`shift+PgUp`). Pure delegate.
    pub fn toggle_mark_page(&mut self, n: usize, downward: bool) {
        self.state.toggle_mark_page(n, downward);
    }

    /// Krusader `Shift+Home`: marks from the cursor upward and unmarks the
    /// rest. Pure delegate.
    pub fn mark_to_top(&mut self) {
        self.state.mark_to_top();
    }

    /// Krusader `Shift+End`: marks from the cursor downward and unmarks the
    /// rest. Pure delegate.
    pub fn mark_to_bottom(&mut self) {
        self.state.mark_to_bottom();
    }

    /// Marks every visible entry. Pure delegate (#103).
    pub fn mark_all(&mut self) {
        self.state.mark_all();
    }

    /// Marks (or unmarks) the ones sharing the cursor's extension. Pure
    /// delegate (#313).
    pub fn mark_same_extension(&mut self, mark: bool) -> usize {
        self.state.mark_same_extension(mark)
    }

    /// Marks the visible ones that are directories (`dirs`) or the ones
    /// that aren't. Pure delegate (#313).
    pub fn mark_kind(&mut self, dirs: bool) -> usize {
        self.state.mark_kind(dirs)
    }

    /// Returns the selection from before the last bulk gesture. Pure
    /// delegate (#313).
    pub fn restore_previous_marks(&mut self) -> Option<usize> {
        self.state.restore_previous_marks()
    }

    /// How many times this pane's listing has MOVED indices — pure delegate
    /// to [`norte_frontend::PaneState::listing_epoch`]. Read by the mouse to
    /// drop a gesture whose indices no longer name what got painted.
    #[must_use]
    pub fn listing_epoch(&self) -> u64 {
        self.state.listing_epoch()
    }

    /// Marks (or unmarks) ONE entry by its index. Pure delegate to the
    /// primitive ctrl+click needs
    /// ([`norte_frontend::PaneState::set_mark`]).
    pub fn set_mark(&mut self, index: usize, marked: bool) {
        self.state.set_mark(index, marked);
    }

    /// Marks the range between two indices, inclusive and in either order;
    /// returns how many marks it changed. ADDITIVE. Pure delegate to
    /// [`norte_frontend::PaneState::mark_range`].
    pub fn mark_range(&mut self, from: usize, to: usize) -> usize {
        self.state.mark_range(from, to)
    }

    /// Arms a pointer sweep. Pure delegate to
    /// [`norte_frontend::PaneState::begin_sweep`].
    pub fn begin_sweep(&mut self) {
        self.state.begin_sweep();
    }

    /// Sets a sweep's CURRENT extent (rubber-band: returns what it stops
    /// covering). Pure delegate to
    /// [`norte_frontend::PaneState::apply_sweep`].
    pub fn apply_sweep(&mut self, from: usize, to: usize) -> usize {
        self.state.apply_sweep(from, to)
    }

    /// Returns what the current sweep marked, leaving it armed. Pure
    /// delegate to [`norte_frontend::PaneState::revert_sweep`].
    pub fn revert_sweep(&mut self) {
        self.state.revert_sweep();
    }

    /// Closes a sweep, dropping its baseline. Pure delegate to
    /// [`norte_frontend::PaneState::end_sweep`].
    pub fn end_sweep(&mut self) {
        self.state.end_sweep();
    }

    /// Inverts the visible entries' marks. Pure delegate (#103).
    pub fn invert_marks(&mut self) {
        self.state.invert_marks();
    }

    /// Removes every mark. Pure delegate (#103).
    pub fn clear_marks(&mut self) {
        self.state.clear_marks();
    }

    /// Marks/unmarks by glob; returns how many marks it changed (#103).
    ///
    /// # Errors
    /// If the pattern doesn't compile.
    pub fn mark_glob(
        &mut self,
        pattern: &str,
        mark: bool,
    ) -> Result<usize, norte_frontend::PatternError> {
        self.state.mark_glob(pattern, mark)
    }

    /// How many marked entries. Pure delegate (#103).
    #[must_use]
    pub fn marks_len(&self) -> usize {
        self.state.marks_len()
    }

    /// Total size of the marked FILES. Pure delegate (#103).
    #[must_use]
    pub fn marked_bytes(&self) -> u64 {
        self.state.marked_bytes()
    }

    /// How many marked entries are directories. Pure delegate (#103) — see
    /// [`norte_frontend::PaneState::marked_dirs`].
    #[must_use]
    pub fn marked_dirs(&self) -> usize {
        self.state.marked_dirs()
    }

    /// Marks the last refresh in the same directory dropped because their
    /// entry disappeared. Pure delegate (#103) — see
    /// [`norte_frontend::PaneState::pruned_marks`].
    #[must_use]
    pub fn pruned_marks(&self) -> usize {
        self.state.pruned_marks()
    }

    /// What the action operates on: marks, or the cursor if there are none.
    /// Pure delegate (#103).
    #[must_use]
    pub fn marked_paths(&self) -> Vec<VPath> {
        self.state.marked_paths()
    }

    /// Seeds the marks a handoff carried (phase 9,
    /// [`norte_frontend::PaneState::seed_marks`]).
    pub fn seed_marks(&mut self, paths: impl IntoIterator<Item = VPath>) {
        self.state.seed_marks(paths);
    }

    /// The MARKED entries, without falling back to the cursor. Pure delegate
    /// (#312).
    #[must_use]
    pub fn marked_entries(&self) -> Vec<&Entry> {
        self.state.marked_entries()
    }

    /// Hidden-files toggle (#107); returns the new state. In the virtual
    /// pane it acts on the RESULTS without touching the preference — on
    /// returning to a real listing, `show_hidden_pref` takes over again.
    pub fn toggle_hidden(&mut self) -> bool {
        let now = self.state.toggle_hidden();
        if !self.virtual_search {
            self.show_hidden_pref = now;
        }
        now
    }

    /// Seeds hidden-files visibility from `[ui] show_hidden` (#107): sets
    /// both the preference AND the current state.
    pub fn set_show_hidden(&mut self, show: bool) {
        self.show_hidden_pref = show;
        self.state.set_show_hidden(show);
    }

    /// Are hidden files shown? Pure delegate.
    #[must_use]
    pub fn show_hidden(&self) -> bool {
        self.state.show_hidden()
    }

    /// Entries set aside by hiding (#107). Pure delegate.
    #[must_use]
    pub fn hidden_count(&self) -> usize {
        self.state.hidden_count()
    }

    /// The listing's active sort (#108). Pure delegate.
    #[must_use]
    pub fn sort(&self) -> norte_frontend::SortSpec {
        self.state.sort()
    }

    /// Changes the listing's sort (#108). Pure delegate.
    pub fn set_sort(&mut self, spec: norte_frontend::SortSpec) {
        self.state.set_sort(spec);
    }

    /// Turns the `..` row on or off (`[ui] parent_entry`). Pure delegate.
    pub fn set_parent_row(&mut self, on: bool) {
        self.state.set_parent_row(on);
    }

    /// Is row `i` the parent one? Pure delegate: asked by rendering — to
    /// write `..` instead of the parent's name — and by navigation.
    #[must_use]
    pub fn is_parent_row(&self, i: usize) -> bool {
        self.state.is_parent_row(i)
    }

    /// Where the parent row leads, if there is one. Pure delegate.
    #[must_use]
    pub fn parent_target(&self) -> Option<&VPath> {
        self.state.parent_target()
    }

    /// Installs the resolved batch of decorations (G3b) — see
    /// `PaneState::set_decorations`.
    pub fn set_decorations(
        &mut self,
        decorations: std::collections::HashMap<VPath, norte_frontend::Decoration>,
    ) {
        self.state.set_decorations(decorations);
    }
}

/// The search form and its pieces live in the SHARED crate
/// ([`norte_frontend::search`]): both frontends ask the SAME search, and the
/// day each builds its own params they drift apart silently. It already
/// happened with a search's outcome, and that's why `search_status` exists
/// (ADR 0077).
///
/// Re-exported under the names used here so the rest of the terminal doesn't
/// notice the move: `SearchDialog` is what this screen is called.
pub use norte_frontend::search::{
    SearchField, SearchForm as SearchDialog, SearchKinds, parse_days,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::sort_entries;
    use crate::app::testutil::*;
    use norte_proto::{EntryKind, VPath};

    /// `extend_listing` re-sorts the WHOLE listing (first page + batch).
    #[test]
    fn extend_reordena_todo() {
        let mut first = vec![file("b.txt"), file("d.txt")];
        sort_entries(&mut first);
        let mut p = Pane::new(root(), first);
        p.set_loading(true);
        p.extend_listing(vec![file("a.txt"), file("c.txt")]);
        assert_eq!(names(&p), vec!["a.txt", "b.txt", "c.txt", "d.txt"]);
    }

    /// The cursor re-anchors to the selected PATH, not the index: filling
    /// doesn't move the user's selection under their feet.
    #[test]
    fn extend_reancla_el_cursor_por_path() {
        let mut first = vec![file("m.txt"), file("z.txt")];
        sort_entries(&mut first);
        let mut p = Pane::new(root(), first);
        p.set_cursor(1); // "z.txt"
        // A batch of names arrives that sort BEFORE: z.txt shifts.
        p.extend_listing(vec![file("a.txt"), file("b.txt")]);
        assert_eq!(names(&p), vec!["a.txt", "b.txt", "m.txt", "z.txt"]);
        assert_eq!(
            p.selected().unwrap().path.file_name().unwrap().as_bytes(),
            b"z.txt"
        );
    }

    /// An empty batch changes nothing (end of drain with no queue).
    #[test]
    fn extend_vacio_es_noop() {
        let mut p = Pane::new(root(), vec![file("a.txt")]);
        p.set_cursor(0);
        p.extend_listing(vec![]);
        assert_eq!(names(&p), vec!["a.txt"]);
        assert_eq!(p.cursor(), 0);
    }

    /// `finish_listing` clears the loading flag.
    #[test]
    fn finish_limpia_loading() {
        let mut p = Pane::new(root(), vec![]);
        p.set_loading(true);
        p.finish_listing();
        assert!(!p.loading());
    }

    /// Active filter: `selected()` (F5/F8/F3…'s basis) points at the
    /// selection WITHIN the filter; cancelling restores the full listing
    /// with the real cursor where it was (the filter never moved it).
    #[test]
    fn quick_filter_redirige_seleccion_y_ops() {
        let mut p = pane_con(&["a1", "b", "a2"]);
        p.quick_start(crate::nav::Mode::Filter);
        p.quick_char('a');
        assert_eq!(
            p.selected().unwrap().path,
            vp("mem:///a1"),
            "selected respects the filter"
        );
        p.quick_down();
        assert_eq!(p.selected().unwrap().path, vp("mem:///a2"));
        p.quick_cancel();
        assert_eq!(
            p.selected().unwrap().path,
            vp("mem:///a1"),
            "restored: cursor to the last real one"
        );
    }

    /// Confirming sets the REAL cursor to what's selected in the filter and
    /// closes it (Enter: the next op — cd, view — starts from that cursor).
    #[test]
    fn quick_confirm_fija_el_cursor_real() {
        let mut p = pane_con(&["a1", "b", "a2"]);
        p.quick_start(crate::nav::Mode::Filter);
        p.quick_char('a');
        p.quick_down();
        p.quick_confirm();
        assert!(p.quick().is_none(), "confirming closes quick search");
        // #54: normalized, the real order is [a1, a2, b] — a2 at index 1.
        assert_eq!(p.cursor(), 1, "real cursor = a2's real index");
        assert_eq!(p.selected().unwrap().path, vp("mem:///a2"));
    }

    /// A new batch from the fill re-applies the filter (spec: on new
    /// batches arriving the filter gets re-applied, not frozen).
    #[test]
    fn extend_listing_reaplica_el_filtro() {
        let mut p = pane_con(&["a1"]);
        p.quick_start(crate::nav::Mode::Filter);
        p.quick_char('a');
        p.extend_listing(vec![file("a2"), file("zz")]);
        assert_eq!(
            p.quick_visible().unwrap().len(),
            2,
            "a2 gets in, zz doesn't"
        );
    }

    /// review MAJOR T4: with the filter having NO matches the list paints
    /// empty — Enter must never act on the real cursor's entry (invisible
    /// to the user). `quick_confirm` returns false and doesn't touch the
    /// cursor.
    #[test]
    fn enter_sin_matches_no_actua_sobre_entrada_invisible() {
        let mut p = pane_con(&["a1", "b", "a2"]);
        p.set_cursor(1);
        p.quick_start(crate::nav::Mode::Filter);
        p.quick_char('x'); // zero matches
        assert!(p.selected().is_none(), "no matches means no selection");
        assert!(
            !p.quick_confirm(),
            "confirming with no matches does NOT set a selection"
        );
        assert!(p.quick().is_none(), "quick search does close");
        assert_eq!(p.cursor(), 1, "the real cursor stays untouched");
    }

    /// Jump mode: the listing does NOT change; typing moves the REAL cursor
    /// to the first match and Tab (`quick_next`) to the next one with wrap.
    #[test]
    fn quick_jump_mueve_el_cursor_real() {
        // #54: normalized, the real order is [ab, ac, zz] — ab and ac match.
        let mut p = pane_con(&["ab", "zz", "ac"]);
        p.quick_start(crate::nav::Mode::Jump);
        p.quick_char('a');
        assert_eq!(p.cursor(), 0, "jumps to the first match");
        assert!(
            p.quick_visible().is_none(),
            "in jump the listing stays untouched"
        );
        p.quick_next();
        assert_eq!(p.cursor(), 1, "Tab: next match");
        p.quick_next();
        assert_eq!(p.cursor(), 0, "wrap");
        assert_eq!(p.selected().unwrap().path, vp("mem:///ab"));
    }

    /// `QuickSearch::refresh`'s contract (T1) end to end: `extend_listing`
    /// RE-SORTS the whole listing, so the filter's selection is kept by
    /// PATH, never by index.
    #[test]
    fn extend_con_resort_conserva_seleccion_por_path() {
        let mut p = pane_con(&["a1", "a2"]);
        p.quick_start(crate::nav::Mode::Filter);
        p.quick_char('a');
        p.quick_down(); // selects a2 (real index 1)
        assert_eq!(p.selected().unwrap().path, vp("mem:///a2"));
        // "a0" sorts BEFORE: a2 moves from real index 1 to 2 after sorting.
        p.extend_listing(vec![file("a0")]);
        assert_eq!(
            p.selected().unwrap().path,
            vp("mem:///a2"),
            "the selection stays on the SAME path after re-sorting"
        );
    }

    /// T4's edge case (review): in Jump with a query with NO matches the
    /// listing paints WHOLE — the real cursor is visible by definition, so
    /// Enter CAN operate on it (in Filter it's still `false`).
    #[test]
    fn enter_en_jump_sin_matches_opera_sobre_el_cursor_visible() {
        let mut p = pane_con(&["a1", "b"]);
        p.set_cursor(1);
        p.quick_start(crate::nav::Mode::Jump);
        p.quick_char('x'); // zero matches; the listing didn't change
        assert!(
            p.quick_confirm(),
            "in Jump the real cursor IS visible: Enter operates"
        );
        assert!(p.quick().is_none(), "quick search closes");
        assert_eq!(p.cursor(), 1, "the real cursor stays where it was");

        // With an EMPTY pane, not even Jump confirms (nothing visible).
        let mut empty = pane_con(&[]);
        empty.quick_start(crate::nav::Mode::Jump);
        assert!(
            !empty.quick_confirm(),
            "with no entries there's nothing to operate on"
        );
    }

    /// #103 review MAJOR-4: `Pane` delegates the mark API to `PaneState`
    /// without reimplementing anything — but the starting set has to be
    /// ASYMMETRIC at each step, or `mark_all`/`invert_marks`/`clear_marks`
    /// become indistinguishable from each other (e.g. over an empty set,
    /// `mark_all` and `invert_marks` give the same result). Each assertion
    /// below would fail if that call were swapped for ANY other delegate.
    #[test]
    fn pane_delegates_the_mark_api() {
        let mut p = Pane::new(
            VPath::parse("mem:///").unwrap(),
            vec![
                e("mem:///a", EntryKind::File),
                e("mem:///b", EntryKind::File),
                e("mem:///c", EntryKind::File),
            ],
        );
        let a = p.entries()[0].clone();
        let b = p.entries()[1].clone();
        let c = p.entries()[2].clone();

        // toggle_mark: marks ONLY the entry under the cursor ("a").
        p.toggle_mark();
        assert_eq!(p.marks_len(), 1);
        assert!(p.is_marked(&a) && !p.is_marked(&b) && !p.is_marked(&c));

        // mark_all from {a}: all THREE, "a" included — if this called
        // invert_marks instead, "a" would get unmarked and the total would
        // be 2.
        p.mark_all();
        assert_eq!(p.marks_len(), 3);
        assert!(p.is_marked(&a) && p.is_marked(&b) && p.is_marked(&c));

        // Reset to an asymmetric set again to be able to tell invert apart.
        p.clear_marks();
        p.toggle_mark(); // {a}

        // invert_marks from {a}: exactly THE OTHER TWO — neither the empty
        // set clear_marks would give, nor the three mark_all would give.
        p.invert_marks();
        assert_eq!(p.marks_len(), 2);
        assert!(!p.is_marked(&a) && p.is_marked(&b) && p.is_marked(&c));

        // clear_marks from {b, c}: empty — invert_marks here would give {a}
        // (marks_len 1), mark_all would give 3.
        p.clear_marks();
        assert_eq!(p.marks_len(), 0);
    }

    /// `mark.toggle`'s real dispatch (main.rs) is a SINGLE call to
    /// `toggle_mark_and_advance` (#103 review MAJOR-2: the "mark + advance"
    /// composition is no longer split into two dispatch calls — it lives
    /// whole in the shared model, which decides whether to advance inside
    /// the filter or over the real cursor; see
    /// `norte_frontend::pane::tests::toggle_mark_and_advance_stays_inside_an_active_filter`
    /// for the filtered case). `dispatch` itself isn't testable here without
    /// a real daemon (it needs `&Backend`/`&mut EventStream`), so this test
    /// pins the same call at `Pane`'s level: marks AND advances, and on the
    /// last row it doesn't wrap.
    #[test]
    fn mark_toggle_advances_without_wrapping_at_the_end() {
        let mut p = Pane::new(
            VPath::parse("mem:///").unwrap(),
            vec![
                e("mem:///a", EntryKind::File),
                e("mem:///b", EntryKind::File),
            ],
        );
        let a = p.entries()[0].clone();
        let b = p.entries()[1].clone();
        assert_eq!(p.cursor(), 0);

        p.toggle_mark_and_advance();
        assert_eq!(p.marks_len(), 1);
        assert!(p.is_marked(&a), "row 0 got marked");
        assert_eq!(p.cursor(), 1, "the cursor advanced after marking");

        // Last row: toggling + advancing must NOT wrap to 0.
        p.toggle_mark_and_advance();
        assert_eq!(p.marks_len(), 2);
        assert!(p.is_marked(&b), "row 1 (last) also got marked");
        assert_eq!(p.cursor(), 1, "clamped on the last row, doesn't wrap");
    }

    /// #107 review MAJOR-1: a search's hits are EXPLICIT — the virtual pane
    /// suspends the hidden-files filter. With `[ui] show_hidden = false`,
    /// searching "env" MUST show `.env`: swallowing it silently (while the
    /// hit counter said 1 over an empty-looking pane) was the bug. On
    /// returning to a real listing, the preference takes over again.
    #[test]
    fn el_pane_virtual_de_busqueda_ensena_hits_ocultos() {
        let mut p = Pane::new(
            VPath::parse("mem:///").unwrap(),
            vec![
                e("mem:///.env", EntryKind::File),
                e("mem:///a", EntryKind::File),
            ],
        );
        p.set_show_hidden(false); // config seed: hide
        assert_eq!(p.entries().len(), 1, "the real listing filters");

        p.begin_search(VPath::parse("mem:///").unwrap());
        p.extend_listing(vec![e("mem:///sub/.env", EntryKind::File)]);
        assert_eq!(
            p.entries().len(),
            1,
            "the hidden hit IS visible in the virtual pane"
        );

        // Ctrl+H inside the virtual pane filters the RESULTS…
        p.toggle_hidden();
        assert_eq!(p.entries().len(), 0);

        // …but does NOT touch the preference: the real listing comes back
        // filtering (and a toggle on the real one does change it).
        p.set_listing(
            VPath::parse("mem:///").unwrap(),
            vec![
                e("mem:///.env", EntryKind::File),
                e("mem:///a", EntryKind::File),
            ],
        );
        assert_eq!(p.entries().len(), 1, "the preference (hide) takes over");
        p.toggle_hidden();
        assert_eq!(p.entries().len(), 2, "real toggle: show");
        p.begin_listing(
            VPath::parse("mem:///sub").unwrap(),
            vec![
                e("mem:///sub/.git", EntryKind::Dir),
                e("mem:///sub/x", EntryKind::File),
            ],
            false,
            None,
        );
        assert_eq!(
            p.entries().len(),
            2,
            "begin_listing respects the new preference (show)"
        );
    }

    /// Tab walks all seven fields and wraps back to the first.
    #[test]
    fn tab_da_la_vuelta_entera() {
        let mut d = SearchDialog::new();
        let first = d.field;
        for _ in 0..SearchField::ORDEN.len() {
            d.toggle_field();
        }
        assert_eq!(d.field, first, "a full loop");
        // And each stop writes into ITS field, which is what rendering
        // assumes when marking the cursor.
        for f in SearchField::ORDEN {
            d.field = f;
            d.push_char('x');
            assert!(d.texto(f).ends_with('x'), "{f:?} didn't receive the key");
        }
    }

    /// Names to exclude are split by commas and trimmed.
    #[test]
    fn los_nombres_a_excluir_se_parten_por_comas() {
        let mut d = SearchDialog::new();
        d.exclude = " target , node_modules ,, .git ".into();
        assert_eq!(d.exclude_names(), ["target", "node_modules", ".git"]);
    }
}
