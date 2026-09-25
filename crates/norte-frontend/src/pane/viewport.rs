//! The viewport: how many rows the previous frame painted, and what follows
//! from that.
//!
//! Pagination and the `stat` probe's radius come from here and not from
//! constants, which would lie in any terminal or window that did not measure
//! exactly that.

use super::{DEFAULT_PAGE, EntryKind, PaneState, VPath};

impl PaneState {
    /// Listing rows this pane painted on the LAST frame (#124): the real
    /// height is decided by the widget when it paints, so the frontend
    /// reports it back here and the model stops guessing it. `None` until the
    /// first frame (or if the pane was not painted at all: with the viewer
    /// open, e.g.).
    pub fn set_viewport_rows(&mut self, rows: usize) {
        self.viewport_rows = (rows > 0).then_some(rows);
    }

    /// Leaves the viewport ready to paint `rows` rows with the cursor where
    /// it is: fixes the height and DRAGS the offset only if the cursor has
    /// gone off it.
    ///
    /// This is the rule of an orthodox file manager, and of any list a user
    /// already has their fingers trained on: moving down inside the screen
    /// does NOT move the content; touching the bottom edge moves it by ONE
    /// row; and moving back up does the symmetric thing. What was here before
    /// was a pure function of the cursor, so the cursor stayed pinned to the
    /// last row and the content moved every time.
    ///
    /// It also reframes without moving the cursor: a listing that shrinks
    /// —a reload, a filter— or a terminal that grows taller would leave the
    /// viewport pointing past the end, with blank rows under content that
    /// does exist.
    pub fn reconcile_viewport(&mut self, rows: usize) {
        self.set_viewport_rows(rows);
        self.viewport_offset = crate::viewport::sticky_offset(
            self.viewport_offset,
            self.cursor,
            self.entries.len(),
            rows,
        );
    }

    /// The first visible row of the listing — see [`Self::reconcile_viewport`].
    #[must_use]
    pub fn viewport_offset(&self) -> usize {
        self.viewport_offset
    }

    /// Visible rows from the last frame (#124) — see [`Self::set_viewport_rows`].
    #[must_use]
    pub fn viewport_rows(&self) -> Option<usize> {
        self.viewport_rows
    }

    /// How many rows a page moves (#124): one SCREEN minus one row of
    /// context, like orthodox file managers — never less than one. With no
    /// frame painted yet it falls back to [`DEFAULT_PAGE`].
    #[must_use]
    pub fn page_step(&self) -> usize {
        self.viewport_rows
            .map_or(DEFAULT_PAGE, |r| r.saturating_sub(1).max(1))
    }

    /// Paths that are candidates for [`Self::hydrate`] in the VISIBLE
    /// viewport: `File` entries with no `size` within `radius` rows of the
    /// cursor (#52, lazy listing). A model SHARED by both frontends (rule 7):
    /// probing only the FOCUSED entry left the Size/Date columns blank on
    /// every other row, which is exactly what an orthodox file manager has to
    /// show.
    ///
    /// The radius comes from the REAL height of the last frame
    /// ([`Self::set_viewport_rows`], #124) and falls back to `fallback`
    /// while there is none yet. A radius equal to the height COVERS the
    /// whole screen whatever the scroll is —what is visible always falls
    /// inside `cursor ± height`— and it also preloads one screen in each
    /// direction, so scrolling does not expose fresh blank cells. A `Dir` is
    /// never probed (its size cell is blank on purpose) and an entry that is
    /// already hydrated stops being a candidate on its own — the caller does
    /// not need to track more state than the dedup of what it ALREADY asked
    /// for (a failed stat would otherwise be retried in a loop).
    #[must_use]
    pub fn needs_stat_window(&self, fallback: usize) -> Vec<VPath> {
        let radius = self.viewport_rows.unwrap_or(fallback);
        let lo = self.cursor.saturating_sub(radius);
        let hi = self.cursor.saturating_add(radius).saturating_add(1);
        self.needs_stat_at(lo..hi)
    }

    /// [`Self::needs_stat_window`] over explicit ABSOLUTE indices: a frontend
    /// that knows its EXACT visible range does not have to approximate it
    /// with a radius around the cursor. The GUI gets it from `uniform_list`
    /// (which only asks for the rows it is about to paint), so a wheel
    /// scroll —which moves the viewport WITHOUT moving the cursor— keeps
    /// hydrating what is visible. Indices outside the listing are ignored.
    #[must_use]
    pub fn needs_stat_at(&self, indices: impl IntoIterator<Item = usize>) -> Vec<VPath> {
        indices
            .into_iter()
            .filter_map(|i| self.entries.get(i))
            .filter(|e| e.kind == EntryKind::File && e.size.is_none())
            .map(|e| e.path.clone())
            .collect()
    }
}
