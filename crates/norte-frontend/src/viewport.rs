//! The visible window of a long list, and the one rule that moves it.
//!
//! It lives apart because THREE lists share it — the file listing, the diff
//! pane and a sync plan's steps — and all three had the same defect: the
//! scroll offset was derived from the cursor (`selected - (height-1)`), so
//! past the first screen the cursor stayed PINNED to the last row and the
//! content moved on every keystroke. A rule written three times is a rule
//! that gets fixed once and stays wrong in the other two.

/// The window that must be painted: the previous one, dragged just enough for
/// `cursor` to fit.
///
/// Pure and tested apart because it is the whole rule: the cursor moves
/// INSIDE the window, and only when it steps outside does the window follow
/// it, one row at a time. The two clamps below matter just as much — a window
/// that survives a shorter list would paint blank space below rows that
/// exist.
#[must_use]
pub fn sticky_offset(previous: usize, cursor: usize, total: usize, rows: usize) -> usize {
    if rows == 0 || total == 0 {
        return 0;
    }
    // Never past what there is: when the list shrinks (or the terminal
    // grows) the window re-frames itself without touching the cursor.
    let cap = total.saturating_sub(rows);
    let mut off = previous.min(cap);
    let cursor = cursor.min(total - 1);
    if cursor < off {
        // Stepped out above: the window starts at it.
        off = cursor;
    } else if cursor >= off + rows {
        // Below: it lands on the LAST row, which is what makes moving down
        // from the edge shift exactly one row.
        off = cursor + 1 - rows;
    }
    off
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #108 L7: `set_sort` re-sorts in place, re-anchors the cursor by PATH
    /// and does not touch the marks (they go by identity); `extend` under the
    /// active spec merges into the new order.
    // TODO(translation): review — this line does not describe the test below
    // it (sticky_offset has nothing to do with set_sort/marks); kept as
    // found, translated as is.
    /// The sticky window, which is the whole rule for the listing's scroll.
    ///
    /// What broke and why it is noticeable: the offset was derived from the
    /// cursor (`selected - (height-1)`), meaning that past the first screen
    /// the cursor lived PINNED to the last row and every keystroke moved the
    /// content. Scrolling back up, the list came down with it and the cursor
    /// never came unstuck from the edge — which is exactly what feels wrong.
    #[test]
    fn window_only_moves_when_the_cursor_touches_an_edge() {
        // Ten rows of window over a hundred.
        // Moving down INSIDE does not move it.
        assert_eq!(sticky_offset(0, 5, 100, 10), 0);
        assert_eq!(sticky_offset(0, 9, 100, 10), 0, "the last visible row");
        // Touching the bottom edge moves it ONE row.
        assert_eq!(sticky_offset(0, 10, 100, 10), 1);
        // And moving up inside the window does not move it either: the
        // cursor moves up on its own.
        assert_eq!(sticky_offset(20, 25, 100, 10), 20);
        assert_eq!(sticky_offset(20, 20, 100, 10), 20, "the first visible row");
        // Until it touches the top edge.
        assert_eq!(sticky_offset(20, 19, 100, 10), 19);
        // A long jump (Home/End, a search hit) re-frames in one go.
        assert_eq!(sticky_offset(20, 0, 100, 10), 0);
        assert_eq!(sticky_offset(20, 99, 100, 10), 90);
    }

    /// And it does not survive a list that shrinks or a terminal that grows:
    /// a window past the end paints blank space below rows that exist.
    #[test]
    fn window_reframes_without_moving_the_cursor() {
        // The list goes from 100 to 12 rows with the window at 90.
        assert_eq!(sticky_offset(90, 5, 12, 10), 2, "cap = total - height");
        // The terminal grows: everything fits and there is nothing to shift.
        assert_eq!(sticky_offset(90, 5, 12, 20), 0);
        // Edge cases: no rows or no window, no offset.
        assert_eq!(sticky_offset(7, 3, 0, 10), 0);
        assert_eq!(sticky_offset(7, 3, 100, 0), 0);
    }
}
