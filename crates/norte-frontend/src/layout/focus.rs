//! Focus traversal, which only goes through what is on screen.

use super::{Resolved, SlotId};

/// The next focusable slot, cycling.
///
/// If `actual` is no longer in `focus_order` — its tab got hidden, or its
/// `Split` collapsed — returns the FIRST one: focus is never lost while
/// there is something to focus, because a focus pointing at something not
/// shown is a keyboard that does nothing and a user who does not know why.
#[must_use]
pub fn focus_next(resolved: &Resolved, actual: SlotId) -> Option<SlotId> {
    let order = &resolved.focus_order;
    match order.iter().position(|id| *id == actual) {
        Some(i) => order.get((i + 1) % order.len()).copied(),
        None => order.first().copied(),
    }
}

/// Like [`focus_next`], backwards.
#[must_use]
pub fn focus_prev(resolved: &Resolved, actual: SlotId) -> Option<SlotId> {
    let order = &resolved.focus_order;
    match order.iter().position(|id| *id == actual) {
        Some(i) => order.get((i + order.len() - 1) % order.len()).copied(),
        None => order.first().copied(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::Rect;

    fn resolved(order: &[u32]) -> Resolved {
        Resolved {
            placements: order
                .iter()
                .map(|i| (SlotId(*i), Rect::new(0, 0, 10, 10)))
                .collect(),
            hidden: vec![],
            focus_order: order.iter().map(|i| SlotId(*i)).collect(),
            diagnostics: vec![],
        }
    }

    #[test]
    fn el_foco_cicla_en_los_dos_sentidos() {
        let r = resolved(&[1, 2, 3]);
        assert_eq!(focus_next(&r, SlotId(3)), Some(SlotId(1)));
        assert_eq!(focus_prev(&r, SlotId(1)), Some(SlotId(3)));
        assert_eq!(focus_next(&r, SlotId(1)), Some(SlotId(2)));
        assert_eq!(focus_prev(&r, SlotId(3)), Some(SlotId(2)));
    }

    /// Focus was on a slot that just got hidden (tab switch, or the window
    /// shrank and collapsed it): it is NOT lost, it falls to the first
    /// visible one.
    #[test]
    fn un_foco_que_ya_no_se_ve_cae_al_primer_visible() {
        let r = resolved(&[2, 3]);
        assert_eq!(focus_next(&r, SlotId(1)), Some(SlotId(2)));
        assert_eq!(focus_prev(&r, SlotId(1)), Some(SlotId(2)));
    }

    #[test]
    fn sin_nada_enfocable_no_hay_foco() {
        let r = resolved(&[]);
        assert_eq!(focus_next(&r, SlotId(1)), None);
        assert_eq!(focus_prev(&r, SlotId(1)), None);
    }

    /// With ONE single slot, cycling stays on it. Seems obvious and is not:
    /// a wrongly written `(i + 1) % 1` returns `None` and the switch-pane key
    /// stops responding when there is only one.
    #[test]
    fn con_un_solo_hueco_ciclar_se_queda_en_el() {
        let r = resolved(&[7]);
        assert_eq!(focus_next(&r, SlotId(7)), Some(SlotId(7)));
        assert_eq!(focus_prev(&r, SlotId(7)), Some(SlotId(7)));
    }
}
