//! Which row of the processes panel is chosen, and nothing else.
//!
//! The rows are the ones from the task board the strip already paints: the
//! panel keeps no second copy, because two task lists drift apart and the one
//! that is seen stops being the one that gets cancelled.
//!
//! The chosen one is what gets cancelled (`task.cancel`) and, since protocol
//! 0.82, paused and resumed (`task.pause`, ADR 0147).
//!
//! It lives here and not in a frontend because what is inside is not about
//! painting: it is the answer to "which task would be stopped?", and that
//! question has to have a single answer in the terminal and in the window
//! (ADR 0077). Written twice, it was already starting to drift.
//!
//! # A position does not name a task
//!
//! The board moves on its own: a finished task is swept after ten seconds,
//! and an unrelated one that no longer fits gets evicted from the front.
//! Storing "row 1" and clamping it on read avoids the index panic, but not
//! the worse failure: if the one that leaves was ABOVE it, row 1 comes to
//! name a different task without the reader touching anything, and the
//! cancel key stops a task nobody chose.
//!
//! That is why what gets stored is the chosen task's IDENTITY, and the
//! position is only the fallback for when that identity is no longer there.
//! It is the same distinction the listing makes between a `RowKey` and an
//! index.

/// The percentage that belongs to a listing ROW, if some task is working on
/// it (spec 2026-09-15, phase 2).
///
/// What is passed in are the live tasks' operands with their percentage —
/// each frontend keeps the board in its own type, and this function does not
/// need to know it. The match is by EXACT path, byte for byte: a copy working
/// inside a directory does not paint the directory as half-done, because
/// "half of this folder" is not what the number says.
///
/// With two tasks on the same row, the LEAST advanced one wins: what is left
/// for that row to settle down is whatever is left for the most behind one.
///
/// ```
/// use norte_frontend::processes::progress_for;
/// use norte_proto::VPath;
/// let vp = |s: &str| VPath::parse(s).unwrap();
/// let tasks = [(vp("mem:///a"), Some(30_u8)), (vp("mem:///a"), Some(70)), (vp("mem:///b"), None)];
/// let iter = || tasks.iter().map(|(p, pct)| (p, *pct));
/// assert_eq!(progress_for(iter(), &vp("mem:///a")), Some(30), "the most behind one wins");
/// assert_eq!(progress_for(iter(), &vp("mem:///b")), None, "no percentage, nothing to paint");
/// assert_eq!(progress_for(iter(), &vp("mem:///c")), None);
/// ```
#[must_use]
pub fn progress_for<'a>(
    tasks: impl IntoIterator<Item = (&'a norte_proto::VPath, Option<u8>)>,
    row: &norte_proto::VPath,
) -> Option<u8> {
    tasks
        .into_iter()
        .filter(|(path, _)| *path == row)
        .filter_map(|(_, pct)| pct)
        .min()
}

/// Which task the processes panel's cursor is on.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Processes {
    /// Where it was, for when the chosen one is no longer there.
    cursor: usize,
    /// Which one is chosen. `None` before anything has moved.
    anchored: Option<u64>,
}

impl Processes {
    /// The row chosen over `ids`, or `None` if the board is empty.
    ///
    /// `ids` are the PAINTED tasks, in the order they are painted. Identity
    /// wins: as long as the chosen one stays on the board, the row is its
    /// own wherever it is. Once it is gone, it falls back to the last known
    /// position, clamped — which is what makes sweeping the last one leave
    /// the selection on the one that is now last, and not all the way at the
    /// top.
    ///
    /// ```
    /// use norte_frontend::processes::Processes;
    /// let mut p = Processes::default();
    /// p.mover(1, &[10, 11, 12]);
    /// assert_eq!(p.row(&[10, 11, 12]), Some(1));
    /// // The 10 expires, and it was ABOVE: the chosen one is still the 11.
    /// assert_eq!(p.row(&[11, 12]), Some(0));
    /// // And if the chosen one leaves, the clamped position wins.
    /// assert_eq!(p.row(&[12]), Some(0));
    /// assert_eq!(p.row(&[]), None);
    /// ```
    #[must_use]
    pub fn row(&self, ids: &[u64]) -> Option<usize> {
        if ids.is_empty() {
            return None;
        }
        if let Some(anchored) = self.anchored
            && let Some(i) = ids.iter().position(|id| *id == anchored)
        {
            return Some(i);
        }
        Some(self.cursor.min(ids.len() - 1))
    }

    /// The same, but `0` with an empty board, to index without a branch.
    ///
    /// The difference from [`Self::row`] is not cosmetic, and that is why
    /// there are two: what crosses the bridge is the optional one, because an
    /// index with no row behind it paints a highlight over nothing.
    ///
    /// ```
    /// use norte_frontend::processes::Processes;
    /// let p = Processes::default();
    /// assert_eq!(p.row_or_zero(&[]), 0);
    /// assert_eq!(p.row(&[]), None);
    /// ```
    #[must_use]
    pub fn row_or_zero(&self, ids: &[u64]) -> usize {
        self.row(ids).unwrap_or(0)
    }

    /// Moves up one row.
    ///
    /// ```
    /// use norte_frontend::processes::Processes;
    /// let mut p = Processes::default();
    /// p.mover(2, &[10, 11, 12]);
    /// p.up(&[10, 11, 12]);
    /// assert_eq!(p.row(&[10, 11, 12]), Some(1));
    /// ```
    pub fn up(&mut self, ids: &[u64]) {
        self.mover(-1, ids);
    }

    /// Moves down one row, without going past the last one.
    ///
    /// ```
    /// use norte_frontend::processes::Processes;
    /// let mut p = Processes::default();
    /// p.down(&[10, 11]);
    /// p.down(&[10, 11]);
    /// p.down(&[10, 11]);
    /// assert_eq!(p.row(&[10, 11]), Some(1), "does not go past the bottom");
    /// ```
    pub fn down(&mut self, ids: &[u64]) {
        self.mover(1, ids);
    }

    /// Moves `delta` rows at once, without going past either end.
    ///
    /// With an EMPTY board it touches nothing: the panel opens with the
    /// system at rest and keystrokes keep arriving, and erasing what is
    /// remembered there would make opening the panel for a moment with no
    /// tasks lose the selection.
    ///
    /// ```
    /// use norte_frontend::processes::Processes;
    /// let mut p = Processes::default();
    /// p.mover(10, &[10, 11, 12, 13]);
    /// assert_eq!(p.row(&[10, 11, 12, 13]), Some(3));
    /// p.mover(3, &[]);
    /// assert_eq!(p.row(&[10, 11, 12, 13]), Some(3), "with no rows it is not forgotten");
    /// p.mover(-10, &[10, 11, 12, 13]);
    /// assert_eq!(p.row(&[10, 11, 12, 13]), Some(0));
    /// ```
    pub fn mover(&mut self, delta: i64, ids: &[u64]) {
        let Some(current) = self.row(ids) else {
            return;
        };
        let current = i64::try_from(current).unwrap_or(i64::MAX);
        let last = i64::try_from(ids.len() - 1).unwrap_or(i64::MAX);
        let target = current.saturating_add(delta).clamp(0, last);
        self.cursor = usize::try_from(target).unwrap_or(0);
        self.anchored = ids.get(self.cursor).copied();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The case position alone cannot answer: the one that leaves was ABOVE
    /// the chosen one. With a clamped index, row 1 used to come to name a
    /// different task without anyone touching anything, and cancelling
    /// stopped a task the reader had not chosen.
    #[test]
    fn expiring_a_row_from_above_does_not_change_the_chosen_task() {
        let mut p = Processes::default();
        p.mover(1, &[10, 11, 12, 13]);
        assert_eq!(p.row(&[10, 11, 12, 13]), Some(1));
        assert_eq!(
            p.row(&[11, 12, 13]),
            Some(0),
            "the 11 is still the 11, now on a different row"
        );
        assert_eq!(
            p.row(&[12, 13]),
            Some(1),
            "without the 11, the remembered position wins, which was 1"
        );
    }

    #[test]
    fn the_cursor_does_not_go_past_the_bottom() {
        let mut p = Processes::default();
        for _ in 0..3 {
            p.down(&[10, 11]);
        }
        assert_eq!(p.row(&[10, 11]), Some(1));
    }

    #[test]
    fn the_cursor_does_not_go_past_the_top() {
        let mut p = Processes::default();
        p.up(&[10, 11, 12]);
        assert_eq!(p.row(&[10, 11, 12]), Some(0));
    }

    /// With no rows there is no valid row, and this is what avoids the index
    /// panic in a panel opened with the system at rest.
    #[test]
    fn no_rows_means_no_row() {
        let p = Processes::default();
        assert_eq!(p.row(&[]), None);
        assert_eq!(p.row_or_zero(&[]), 0);
    }

    /// The position is REMEMBERED when the chosen one is no longer there: if
    /// the board shrinks and grows back, the row returns to where it was
    /// instead of staying stuck at the top.
    #[test]
    fn shrinking_does_not_erase_where_the_row_was() {
        let mut p = Processes::default();
        let all = [10, 11, 12, 13, 14, 15];
        p.mover(4, &all);
        assert_eq!(p.row(&all), Some(4));
        assert_eq!(
            p.row(&[10, 11]),
            Some(1),
            "clamped while there are two rows"
        );
        assert_eq!(
            p.row(&all),
            Some(4),
            "and it returns once there is room again"
        );
    }

    /// Moving up after the board shrinks REALLY moves.
    ///
    /// Subtracting from the stored number — which is what both surfaces used
    /// to do — made the key mute: with the cursor on 8 and three rows,
    /// moving up left it at 7 and it still read the 2.
    #[test]
    fn moving_up_with_a_shrunk_board_moves_one_of_the_visible_rows() {
        let mut p = Processes::default();
        let all: Vec<u64> = (0..10).collect();
        p.mover(8, &all);
        // Both the ones above AND the chosen one leave: the clamped position wins.
        let remaining = [100, 101, 102];
        assert_eq!(p.row(&remaining), Some(2));
        p.up(&remaining);
        assert_eq!(
            p.row(&remaining),
            Some(1),
            "moves up from there, not from 8"
        );
    }

    /// Moving with an empty board is not a rare case, and it must not lose
    /// the selection: the panel stays open and keystrokes keep arriving.
    #[test]
    fn moving_with_no_rows_does_not_forget_what_was_chosen() {
        let mut p = Processes::default();
        p.mover(2, &[10, 11, 12]);
        p.mover(3, &[]);
        p.up(&[]);
        assert_eq!(p.row(&[10, 11, 12]), Some(2));
    }
}
