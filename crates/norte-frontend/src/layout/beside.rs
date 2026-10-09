//! What sits beside a slot ON SCREEN, and where a location travels when it
//! is sent towards a side (`pane.send-left`/`-right`).
//!
//! By geometry and never by index or role: after `pane.swap`, `layout.flip`
//! or a third split, "the right pane" is whatever the reader sees on the
//! right, and both frontends feed the rectangles they actually laid out.

use std::cmp::Reverse;

use super::Rect;

/// A horizontal side of the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// Towards smaller `x`.
    Left,
    /// Towards larger `x`.
    Right,
}

impl Side {
    /// The other side.
    #[must_use]
    pub const fn opposite(self) -> Self {
        match self {
            Self::Left => Self::Right,
            Self::Right => Self::Left,
        }
    }
}

/// Which slot's location travels, and which slot goes there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Travel<K> {
    /// The slot whose CURRENT location is sent.
    pub from: K,
    /// The slot that navigates to it.
    pub to: K,
}

/// The nearest slot on `side` of `from` that shares rows with it.
///
/// `placements` must already hold only the candidates (listings, on
/// screen). Among those strictly on that side and overlapping `from`
/// vertically, the closest horizontally wins; then the one sharing the most
/// rows; then the topmost. Stacked slots share no side, so they get `None`.
#[must_use]
pub fn beside<K: Copy + PartialEq>(placements: &[(K, Rect)], from: K, side: Side) -> Option<K> {
    let (_, f) = placements.iter().find(|(k, _)| *k == from)?;
    let right_edge = |r: &Rect| u32::from(r.x) + u32::from(r.width);
    let bottom = |r: &Rect| u32::from(r.y) + u32::from(r.height);
    placements
        .iter()
        .filter(|(k, _)| *k != from)
        .filter_map(|(k, r)| {
            let gap = match side {
                Side::Right => u32::from(r.x).checked_sub(right_edge(f))?,
                Side::Left => u32::from(f.x).checked_sub(right_edge(r))?,
            };
            let shared = bottom(f)
                .min(bottom(r))
                .saturating_sub(u32::from(f.y.max(r.y)));
            (shared > 0).then_some(((gap, Reverse(shared), r.y), *k))
        })
        .min_by_key(|(rank, _)| *rank)
        .map(|(_, k)| k)
}

/// `pane.send-right`/`-left`: the location travels in the arrow's direction.
///
/// With a neighbour on `side`, it goes to where `focused` is. Without one,
/// `focused` goes to where its neighbour on the OTHER side is — so with two
/// panes side by side the right one always ends where the left one is for
/// `Right`, whichever has the focus. `None` when there is nothing on either
/// side.
#[must_use]
pub fn send_toward<K: Copy + PartialEq>(
    placements: &[(K, Rect)],
    focused: K,
    side: Side,
) -> Option<Travel<K>> {
    if let Some(to) = beside(placements, focused, side) {
        return Some(Travel { from: focused, to });
    }
    beside(placements, focused, side.opposite()).map(|from| Travel { from, to: focused })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two listings side by side, 1 left and 2 right.
    fn two() -> Vec<(u32, Rect)> {
        vec![(1, Rect::new(0, 0, 50, 20)), (2, Rect::new(50, 0, 50, 20))]
    }

    #[test]
    fn with_two_panes_right_always_lands_in_the_right_one() {
        let p = two();
        assert_eq!(
            send_toward(&p, 1, Side::Right),
            Some(Travel { from: 1, to: 2 })
        );
        assert_eq!(
            send_toward(&p, 2, Side::Right),
            Some(Travel { from: 1, to: 2 }),
            "focus on the right: it is still the right pane that travels"
        );
        assert_eq!(
            send_toward(&p, 2, Side::Left),
            Some(Travel { from: 2, to: 1 })
        );
        assert_eq!(
            send_toward(&p, 1, Side::Left),
            Some(Travel { from: 2, to: 1 })
        );
    }

    /// Ids say nothing about sides: a flipped or swapped layout puts slot 2
    /// on the left, and the arrow follows the screen.
    #[test]
    fn the_screen_decides_not_the_id() {
        let p = vec![(1, Rect::new(50, 0, 50, 20)), (2, Rect::new(0, 0, 50, 20))];
        assert_eq!(
            send_toward(&p, 1, Side::Right),
            Some(Travel { from: 2, to: 1 })
        );
        assert_eq!(
            send_toward(&p, 1, Side::Left),
            Some(Travel { from: 1, to: 2 })
        );
    }

    #[test]
    fn with_three_the_middle_one_sends_to_its_immediate_neighbour() {
        let p = vec![
            (1, Rect::new(0, 0, 30, 20)),
            (2, Rect::new(30, 0, 30, 20)),
            (3, Rect::new(60, 0, 30, 20)),
        ];
        assert_eq!(
            send_toward(&p, 2, Side::Right),
            Some(Travel { from: 2, to: 3 })
        );
        assert_eq!(
            send_toward(&p, 2, Side::Left),
            Some(Travel { from: 2, to: 1 })
        );
        assert_eq!(
            send_toward(&p, 1, Side::Right),
            Some(Travel { from: 1, to: 2 }),
            "the NEAREST on that side, not the furthest"
        );
        assert_eq!(
            send_toward(&p, 1, Side::Left),
            Some(Travel { from: 2, to: 1 })
        );
    }

    #[test]
    fn stacked_panes_have_no_side() {
        let p = vec![
            (1, Rect::new(0, 0, 100, 10)),
            (2, Rect::new(0, 10, 100, 10)),
        ];
        assert_eq!(send_toward(&p, 1, Side::Right), None);
        assert_eq!(send_toward(&p, 2, Side::Left), None);
    }

    /// A column split in two beside a full-height pane: from the full one,
    /// the half sharing the most rows wins, and on a tie the top one.
    #[test]
    fn the_largest_vertical_overlap_wins_and_ties_go_to_the_top() {
        let p = vec![
            (1, Rect::new(0, 0, 50, 20)),
            (2, Rect::new(50, 0, 50, 8)),
            (3, Rect::new(50, 8, 50, 12)),
        ];
        assert_eq!(beside(&p, 1, Side::Right), Some(3));
        let tie = vec![
            (1, Rect::new(0, 0, 50, 20)),
            (2, Rect::new(50, 0, 50, 10)),
            (3, Rect::new(50, 10, 50, 10)),
        ];
        assert_eq!(beside(&tie, 1, Side::Right), Some(2));
        assert_eq!(beside(&tie, 3, Side::Left), Some(1));
    }

    #[test]
    fn a_slot_not_placed_has_no_neighbours() {
        assert_eq!(send_toward(&two(), 9, Side::Right), None);
        assert_eq!(
            send_toward(&[(1, Rect::new(0, 0, 9, 9))], 1, Side::Right),
            None
        );
    }
}
