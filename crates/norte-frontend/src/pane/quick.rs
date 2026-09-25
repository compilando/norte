//! A pane's quick search: filter or jump while typing.
//!
//! Kept apart because it has its own lifecycle —it opens, eats keys, gets
//! confirmed or cancelled— and because a new listing closes it: mixed into
//! the rest of the pane, every listing method had to remember it.

use super::{Mode, PaneState, QuickSearch, VPath};
use crate::nav::Match;

impl PaneState {
    /// Starts the quick search in `mode` over the current entries, folding
    /// with the currently active name reinterpretation (#98/F1).
    pub fn quick_start(&mut self, mode: Mode) {
        self.quick = Some(QuickSearch::new(mode, &self.entries, self.name_encoding));
    }

    /// A printable key no binding took, in a preset with `type_to_search`:
    /// opens a quick search that JUMPS to the names starting with it, as
    /// Krusader does. From then on it is an ordinary quick search. A letter
    /// no name starts with opens nothing: an empty search would keep the
    /// arrows until Esc, and the reader never asked for one.
    pub fn type_to_search(&mut self, c: char) {
        let mut q =
            QuickSearch::with_match(Mode::Jump, Match::Prefix, &self.entries, self.name_encoding);
        q.push_char(c);
        if q.visible().is_empty() {
            return;
        }
        self.quick = Some(q);
        self.quick_sync_jump();
    }

    /// In [`Mode::Jump`] the REAL cursor follows the quick search's selection
    /// (the listing does not change; jumping IS moving the cursor). In
    /// Filter, a no-op.
    pub(super) fn quick_sync_jump(&mut self) {
        if let Some(q) = &self.quick
            && q.mode() == Mode::Jump
            && let Some(i) = q.selected_entry_index()
        {
            self.cursor = i;
        }
    }

    /// A character typed while the quick search is active.
    pub fn quick_char(&mut self, c: char) {
        if let Some(q) = &mut self.quick {
            q.push_char(c);
            self.quick_sync_jump();
        }
    }

    /// Backspace while the quick search is active.
    pub fn quick_backspace(&mut self) {
        if let Some(q) = &mut self.quick {
            q.backspace();
            self.quick_sync_jump();
        }
    }

    /// Moves the quick search's selection down one position.
    pub fn quick_down(&mut self) {
        if let Some(q) = &mut self.quick {
            q.down();
            self.quick_sync_jump();
        }
    }

    /// Moves the quick search's selection up one position.
    pub fn quick_up(&mut self) {
        if let Some(q) = &mut self.quick {
            q.up();
            self.quick_sync_jump();
        }
    }

    /// Closes the quick search, setting the REAL cursor to the selection
    /// (Enter: the next op starts from there). Returns `true` if the cursor
    /// points at an entry the user COULD SEE: in Filter with no matches it
    /// returns `false` (the painted list was empty); in Jump it returns
    /// `true` if there are entries (the whole listing is painted, the real
    /// cursor is visible by definition).
    pub fn quick_confirm(&mut self) -> bool {
        let Some(q) = self.quick.take() else {
            return false;
        };
        if let Some(i) = q.selected_entry_index() {
            self.cursor = i;
            return true;
        }
        q.mode() == Mode::Jump && !self.entries.is_empty()
    }

    /// Closes the quick search WITHOUT touching the real cursor: in Filter
    /// the full listing comes back with the cursor where it was; in Jump the
    /// cursor stays wherever it jumped to.
    pub fn quick_cancel(&mut self) {
        self.quick = None;
    }

    /// The REAL indices visible under the filter; `None` = no filter (quick
    /// search inactive, or Jump mode: the whole listing is painted).
    #[must_use]
    pub fn quick_visible(&self) -> Option<&[usize]> {
        self.quick
            .as_ref()
            .filter(|q| q.mode() == Mode::Filter)
            .map(QuickSearch::visible)
    }

    /// Next match with wrap (Tab in [`Mode::Jump`]): moves the quick
    /// search's selection to the next match and, in Jump, drags the real
    /// cursor along. No-op without a quick search. (#82)
    pub fn quick_next(&mut self) {
        if let Some(q) = &mut self.quick {
            q.next_match();
            self.quick_sync_jump();
        }
    }

    /// The live quick search (so the render can paint the query and its
    /// counter); `None` = normal navigation. Read-only. (#82)
    #[must_use]
    pub fn quick(&self) -> Option<&QuickSearch> {
        self.quick.as_ref()
    }

    /// The path of the entry selected INSIDE the quick search, captured
    /// BEFORE mutating/re-ordering `entries` (contract of
    /// [`QuickSearch::refresh`]: indices from before the sort identify
    /// nothing). (#82)
    pub(super) fn quick_selected_path(&self) -> Option<VPath> {
        let i = self.quick.as_ref()?.selected_entry_index()?;
        Some(self.entries.get(i)?.path.clone())
    }

    /// RE-APPLIES the live quick search over the CURRENT entries (without
    /// changing them or moving the real cursor): closes a fill whose closing
    /// might re-order them. (#82)
    pub fn refresh_quick(&mut self) {
        let quick_prev = self.quick_selected_path();
        if let Some(q) = &mut self.quick {
            q.refresh(&self.entries, quick_prev.as_ref());
        }
    }
}
