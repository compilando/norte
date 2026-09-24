//! Layout: tree + area -> who is painted, where, and who is not.

use super::{Dir, KindRegistry, LayoutDiagnostic, Node, Rect, RoleId, Size, SlotId};

/// What a frame needs to know, in a single value.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Resolved {
    /// Who is painted and where, in paint order.
    pub placements: Vec<(SlotId, Rect)>,
    /// Who is NOT painted: an inactive tab, or collapsed away.
    pub hidden: Vec<SlotId>,
    /// The order tabbing follows. Already without hidden subtrees.
    pub focus_order: Vec<SlotId>,
    /// What had to be fixed on the fly.
    pub diagnostics: Vec<LayoutDiagnostic>,
}

/// Lays `area` out among `tree`'s slots.
///
/// Collapsing lives here and DIES with the frame: a `Split` whose children
/// do not reach their declared minimum degrades to tabs for this frame and
/// propagates upward if it still does not fit. The input tree is never
/// touched — a saved layout is the user's intent, and rewriting it for the
/// screen's size would mean opening the TUI for a minute wrecks the GUI's
/// layout (ADR 0058 D5).
#[must_use]
pub fn resolve(area: Rect, tree: &Node, decls: &KindRegistry) -> Resolved {
    let mut out = Resolved::default();
    place(tree, area, decls, &mut out);
    // #229: a screen with no USABLE listing is not a screen, it is a hang
    // with borders. `layout.close-slot` already refuses to close the last
    // listing for this reason; layout could produce exactly that, and this
    // is the other half of the same rule.
    //
    // It happens because FIXED ones charge in full and before the weighted
    // ones, and a `Split` with a fixed child does not collapse (collapsing
    // it would take the chrome with it). With `full` at 40x10 that is 16
    // for the sidebar plus 30 for the right column out of 40 columns: both
    // browsers at zero.
    if listing_usable(&out, tree, decls) || !has_listing(tree, decls) {
        return out;
    }
    // The chrome is set aside FROM LARGEST TO SMALLEST and one at a time,
    // stopping at the first attempt that DOES show a listing: this way the
    // status bar — a one-row fixed — survives an eight-row panel being set
    // aside. The input tree is NOT touched: this lives and dies with the
    // frame, like the collapse (ADR 0058 D5).
    let mut pruned = tree.clone();
    let mut set_aside: Vec<SlotId> = Vec::new();
    let mut short = ejes_short(&out, tree, decls);
    // The largest ONE OF A SHORT AXIS, one per round. Candidates are
    // recomputed over the already-pruned tree — setting a child aside
    // moves its siblings' indices — and so are the axes: setting aside a
    // wide panel can leave the width resolved and the height not. Each
    // round removes one, so it terminates.
    while let Some(chosen) = chrome_high_to_low(&pruned, decls)
        .into_iter()
        .find(|c| match c.eje {
            Dir::Horizontal => short.0,
            Dir::Vertical => short.1,
        })
    {
        let Some(smaller) = sin_camino(&pruned, &chosen.camino) else {
            break;
        };
        pruned = smaller;
        set_aside.extend(chosen.slots.iter().copied());
        let mut attempt = Resolved::default();
        place(&pruned, area, decls, &mut attempt);
        if listing_usable(&attempt, tree, decls) {
            for id in &set_aside {
                attempt
                    .diagnostics
                    .push(LayoutDiagnostic::ChromeSetAside { slot: *id });
            }
            // Set aside is SUSPENDED, not lost: `placements` and `hidden`
            // partition the tree's slots, and a panel that is not painted
            // and not suspended either is left with its watch open.
            attempt.hidden.extend(set_aside);
            return attempt;
        }
        short = ejes_short(&attempt, tree, decls);
    }
    // Setting aside all the chrome does not fix it either: the real layout
    // is kept, which at least respects what the user set. Swapping it for
    // an equally useless screen that also has no chrome helps nobody.
    out
}

/// The CEILING of the "this shows something" floor: twelve columns and four
/// rows.
///
/// Twelve columns is what a short name between borders takes — a `browser`
/// at 12x8 is tight and works, and that is why the `krusader` preset at
/// 40x10 keeps its sidebar and this rule never even notices — four rows are
/// two borders, the header and one entry. A panel below this is not tight:
/// it is empty.
///
/// It is not the kind's minimum, and the difference is what separates
/// "tight" from "empty". A `browser`'s minimum is 20x5 and says when a
/// PROPORTIONAL layout collapses — "below this I do not paint a name with
/// its size" — this says when the panel paints NOTHING.
///
/// It is a CEILING and not the final number: the real floor comes from
/// [`min_visible`], which starts from the kind's minimum. That way a
/// modest kind does not have to pretend it needs twelve columns, and a
/// demanding one — the registry is open — does not drag the rescue into
/// setting chrome aside chasing an impossible size.
const CONTENT: (u16, u16) = (12, 4);

/// A kind's floor: its own minimum, CAPPED by [`CONTENT`].
///
/// The cap is what stops a demanding kind — the registry is open and
/// `insert` is public, so a plugin can declare `min = (40, 8)` — from
/// making the rescue set chrome aside forever chasing a size the screen
/// does not have. The kind's own minimum is what stops the opposite: calling
/// something "usable" at 4 columns when it declared needing forty.
fn min_visible(decls: &KindRegistry, kind: &super::KindId) -> (u16, u16) {
    let (mw, mh) = decls.min_of(kind);
    (mw.min(CONTENT.0), mh.min(CONTENT.1))
}

/// Where a border pair starts and how long it is, on `dir`'s axis: from the
/// start of what `left` placed to the end of what `right` placed (the two
/// sides of [`Node::border_pair`]). `None` if either side has nothing
/// placed.
///
/// A single count for the TUI and the window: measuring only the two slots
/// that touch — and not the layout's whole children — gave another pair's
/// fraction, and the border between a listing and the details did not
/// follow the pointer.
#[must_use]
pub fn border_span(
    res: &Resolved,
    left: &[SlotId],
    der: &[SlotId],
    dir: Dir,
) -> Option<(u16, u16)> {
    let span = |ids: &[SlotId]| {
        res.placements
            .iter()
            .filter(|(id, _)| ids.contains(id))
            .map(|(_, r)| match dir {
                Dir::Horizontal => (r.x, r.x + r.width),
                Dir::Vertical => (r.y, r.y + r.height),
            })
            .reduce(|(a0, a1), (b0, b1)| (a0.min(b0), a1.max(b1)))
    };
    let (start, _) = span(left)?;
    let (_, end) = span(der)?;
    Some((start, end.saturating_sub(start)))
}

/// Does `after`'s layout still show everything `before` showed? (ADR 0138)
///
/// Asked by whoever MOVES or FLIPS a panel before committing to the new
/// tree: dropping a listing below another on a short terminal leaves it
/// with no room, layout hides it and the reader sees their panel vanish.
/// `tolerado` is the one that goes behind a tab on purpose — the target of
/// dropping in the center. Also, if two listings or more were visible
/// before, two have to be visible after: "the other pane" is a copy's
/// target, and with only one visible there is no other.
#[must_use]
pub fn keeps_on_screen(
    before: &Resolved,
    after: &Resolved,
    tree: &Node,
    tolerado: Option<SlotId>,
) -> bool {
    let placed = |r: &Resolved, id: SlotId| r.placements.iter().any(|(s, _)| *s == id);
    let lost = before
        .placements
        .iter()
        .any(|(id, _)| Some(*id) != tolerado && !placed(after, *id));
    let listings = |r: &Resolved| {
        r.placements
            .iter()
            .filter(|(id, _)| tree.kind_of(*id).is_some_and(|k| k.as_str() == "browser"))
            .count()
    };
    !lost && listings(after) >= listings(before).min(2)
}

/// Would TWO slots of `kind` fit if `rect` were split along `dir`?
///
/// Asked by whoever is about to split, BEFORE touching the tree. Without
/// this, splitting a slot that no longer has room for two creates a panel
/// layout hides in the same frame: the `Split` does not fit, degrades to
/// tabs and the screen goes back to showing one — with the tree saving the
/// new one regardless. What the reader sees is a key that does nothing, or
/// worse, that undoes the previous one.
///
/// The count is the SAME one that decides the collapse — the kind's
/// declared minimum, not the content one — and that is why it lives right
/// here: two criteria for the same question drift apart the moment someone
/// touches one.
#[must_use]
pub fn has_room_to_split(rect: Rect, dir: Dir, kind: &super::KindId, decls: &KindRegistry) -> bool {
    let (mw, mh) = decls.min_of(kind);
    match dir {
        Dir::Horizontal => rect.width / 2 >= mw && rect.height >= mh,
        Dir::Vertical => rect.height / 2 >= mh && rect.width >= mw,
    }
}

/// Is there a slot in `out` that can hold role `active` and has room to
/// show something?
fn listing_usable(out: &Resolved, tree: &Node, decls: &KindRegistry) -> bool {
    out.placements.iter().any(|(id, re)| {
        tree.kind_of(*id).is_some_and(|k| {
            let (mw, mh) = min_visible(decls, k);
            decls.holds_role(k, RoleId::Active) && re.width >= mw && re.height >= mh
        })
    })
}

/// Which AXES `out`'s best listing falls short on: `(width, height)`.
///
/// Looked at per axis and not in bulk because chrome is also per axis: a
/// fixed panel inside a vertical `Split` eats height and not width, and
/// setting it aside does not fix a short width. Without this, a `full` at
/// 80x10 — where only HEIGHT is missing — also lost the sidebar and the
/// right column, which were not in the way.
fn ejes_short(out: &Resolved, tree: &Node, decls: &KindRegistry) -> (bool, bool) {
    // An axis is attacked if it is short for ANY listing, not if it is
    // short for all of them.
    //
    // This function is only called when NO listing works, so the question
    // is not "are they all narrow?" but "which of the two axes can I give
    // room to so that some listing works?". With "all", a real screen
    // failed: a wide, one-row listing on top, and a tall, two-column one
    // below. Neither worked, each fell short on a different axis, and
    // since not ALL were narrow nor ALL short, the rescue answered that no
    // axis was missing and returned the broken screen intact.
    let listings = || {
        out.placements.iter().filter_map(|(id, re)| {
            let k = tree.kind_of(*id)?;
            decls
                .holds_role(k, RoleId::Active)
                .then(|| (min_visible(decls, k), re))
        })
    };
    let mut missing = (false, false);
    let mut any = false;
    for ((mw, mh), re) in listings() {
        any = true;
        missing.0 |= re.width < mw;
        missing.1 |= re.height < mh;
    }
    // With no listing placed at all, both axes are in play: what is
    // missing is room, and it is not known which kind.
    if any { missing } else { (true, true) }
}

/// Does the tree have any slot that can hold role `active`?
///
/// A layout that does not — a chrome-only screen — is legal, and for it
/// there is nothing to rescue: it is laid out and painted.
fn has_listing(node: &Node, decls: &KindRegistry) -> bool {
    match node {
        Node::Slot { kind, .. } => decls.holds_role(kind, RoleId::Active),
        Node::Split { children, .. } | Node::Tabs { children, .. } => {
            children.iter().any(|c| has_listing(c, decls))
        }
    }
}

/// The tree's chrome, from largest to smallest.
///
/// **Chrome = a `Fixed`-sized child whose subtree has not even one slot
/// that can hold `active`.** Both halves matter. Fixed, because a weighted
/// one already gives way on its own. And with no listing inside, because a
/// fixed one that IS a listing is not chrome: setting it aside would be
/// taking away one screen to give it to another.
///
/// A child that is already chrome is not entered: it is set aside whole,
/// and its inner fixed ones are not separate decisions.
fn chrome_high_to_low(node: &Node, decls: &KindRegistry) -> Vec<Chrome> {
    let mut found = Vec::new();
    collects_chrome(node, decls, &[], &mut found);
    // Ties resolved by id: a frame's layout cannot depend on the order a
    // `sort_unstable` leaves two panes of the same size in.
    found.sort_by(|a, b| {
        b.declared
            .cmp(&a.declared)
            .then_with(|| a.slots.cmp(&b.slots))
    });
    found
}

/// A docked panel that can be set aside, and which axis it eats room on.
#[derive(Debug, Clone)]
struct Chrome {
    /// The cells it declares on its parent's axis.
    declared: u16,
    /// The axis of the `Split` that contains it: the room it gives back
    /// when set aside.
    eje: Dir,
    /// The child indices from the root down to it.
    ///
    /// By POSITION and not by its `SlotId`, and that is not a detail: a
    /// tree can carry the same id twice — [`super::validate`] rejects it,
    /// but `resolve` has to withstand any tree — and setting aside "the
    /// slots with this id" also took the other copy with it, left unpainted
    /// and unsuspended. A live slot nobody paints and nobody suspends is an
    /// open watch staring at nothing, and the partition property caught it.
    camino: Vec<usize>,
    /// The slots it takes with it.
    slots: Vec<SlotId>,
}

/// Accumulates `node`'s chrome into `found`, carrying the path from the
/// root.
fn collects_chrome(node: &Node, decls: &KindRegistry, here: &[usize], found: &mut Vec<Chrome>) {
    let (Node::Split { children, .. } | Node::Tabs { children, .. }) = node else {
        return;
    };
    // A `Tabs` does not lay out room: its children share it, so none of
    // them is chrome by size and here it only descends to look.
    let eje = if let Node::Split { dir, .. } = node {
        *dir
    } else {
        Dir::Horizontal
    };
    for (i, c) in children.iter().enumerate() {
        let mut camino = here.to_vec();
        camino.push(i);
        match (sizes_get(node, i), has_listing(c, decls)) {
            (Some(Size::Fixed(n)), false) => found.push(Chrome {
                declared: n,
                eje,
                camino,
                slots: c.slot_ids(),
            }),
            _ => collects_chrome(c, decls, &camino, found),
        }
    }
}

/// Child `i`'s size, if its parent lays out by sizes. A `Tabs` does not lay
/// out: its children share the room, so none of them is chrome by size.
fn sizes_get(node: &Node, i: usize) -> Option<Size> {
    match node {
        Node::Split { sizes, .. } => sizes.get(i).copied(),
        Node::Slot { .. } | Node::Tabs { .. } => None,
    }
}

/// The tree without the child `camino` points at, or `None` if nothing is
/// left.
///
/// Private on purpose, and not a method on [`Node`]: the ones there are the
/// user's intentions and are PERSISTED. This is a frame's patch-up, and
/// having it handy where a layout is saved is how the terminal's size ends
/// up erasing someone's sidebar forever.
fn sin_camino(node: &Node, camino: &[usize]) -> Option<Node> {
    let Some((&i, rest)) = camino.split_first() else {
        // Path exhausted: this node IS the one leaving.
        return None;
    };
    match node {
        // A path that goes through a slot does not exist; the tree stays
        // the same.
        Node::Slot { .. } => Some(node.clone()),
        Node::Split {
            dir,
            children,
            sizes,
        } => {
            let mut children_out = Vec::new();
            let mut sizes_out = Vec::new();
            for (j, c) in children.iter().enumerate() {
                let remains = if j == i {
                    sin_camino(c, rest)
                } else {
                    Some(c.clone())
                };
                if let Some(q) = remains {
                    children_out.push(q);
                    sizes_out.push(sizes.get(j).copied().unwrap_or(Size::Weight(1)));
                }
            }
            if children_out.is_empty() {
                return None;
            }
            Some(Node::Split {
                dir: *dir,
                children: children_out,
                sizes: sizes_out,
            })
        }
        Node::Tabs { children, active } => {
            let mut children_out = Vec::new();
            // The active tab is an INDEX: removing one before it shifts
            // every one after it. Simply clamping it left the next one
            // visible and SUSPENDED the one the user was looking at.
            let mut active_idx = *active;
            for (j, c) in children.iter().enumerate() {
                let remains = if j == i {
                    sin_camino(c, rest)
                } else {
                    Some(c.clone())
                };
                if remains.is_none() && j < active_idx {
                    active_idx -= 1;
                }
                if let Some(q) = remains {
                    children_out.push(q);
                }
            }
            if children_out.is_empty() {
                return None;
            }
            Some(Node::Tabs {
                active: active_idx.min(children_out.len() - 1),
                children: children_out,
            })
        }
    }
}

/// What a WHOLE subtree demands to fit.
///
/// A `Split` sums along its axis and takes the max on the other; a `Tabs`
/// takes the max on both, because its children share the same room.
fn min_of(node: &Node, decls: &KindRegistry) -> (u16, u16) {
    match node {
        Node::Slot { kind, .. } => decls.min_of(kind),
        Node::Tabs { children, .. } => children
            .iter()
            .map(|c| min_of(c, decls))
            .fold((0, 0), |(w, h), (cw, ch)| (w.max(cw), h.max(ch))),
        Node::Split {
            dir,
            children,
            sizes,
        } => children
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let (cw, ch) = min_of(c, decls);
                // A FIXED child does not ask for its minimum along the
                // axis: it asks for exactly what it declares, and that
                // wins.
                match (sizes.get(i), dir) {
                    (Some(Size::Fixed(n)), Dir::Horizontal) => (*n, ch),
                    (Some(Size::Fixed(n)), Dir::Vertical) => (cw, *n),
                    (Some(Size::Auto), Dir::Horizontal) => (0, ch),
                    (Some(Size::Auto), Dir::Vertical) => (cw, 0),
                    _ => (cw, ch),
                }
            })
            .fold((0, 0), |(w, h), (cw, ch)| match dir {
                Dir::Horizontal => (w.saturating_add(cw), h.max(ch)),
                Dir::Vertical => (w.max(cw), h.saturating_add(ch)),
            }),
    }
}

/// Lays `area` out among `sizes` along `dir`.
///
/// FIXED ones charge first and in order; if they no longer fit, they are
/// clipped and the weighted ones are left with nothing — which is what
/// makes a tiny terminal keep painting the status bar instead of nothing.
/// The rest is split among the weighted ones, and the remainder of the
/// integer division goes to the LAST WEIGHTED one: with an odd width a
/// column would be lost, and an unpainted column at a pane's edge shows.
///
/// A [`Size::Auto`] counts as zero. It should not reach this far —
/// [`Node::substitute_auto`] replaces it earlier — but a layout pass is no
/// place to blow up.
///
/// `suelos` is each child's minimum on the layout's axis. If the free room
/// is enough for ALL the weighted ones', none goes below its own: it gets
/// its minimum and the rest is redistributed among the others, in
/// proportion. If it is not enough, the layout is the usual proportional
/// one and the collapse decides. The remainder column of the integer
/// division goes to the last weighted one among those that did NOT stay at
/// their floor. Without this a small weight next to large weights — the
/// one that lets a border be dragged — came out one pixel short.
fn distribute(area: Rect, dir: Dir, sizes: &[Size], suelos: &[u16]) -> Vec<Rect> {
    let extent = u64::from(match dir {
        Dir::Horizontal => area.width,
        Dir::Vertical => area.height,
    });
    let mut assigned: Vec<u64> = vec![0; sizes.len()];
    let mut used: u64 = 0;
    for (i, s) in sizes.iter().enumerate() {
        if let Size::Fixed(n) = s {
            let fits = u64::from(*n).min(extent.saturating_sub(used));
            assigned[i] = fits;
            used = used.saturating_add(fits);
        }
    }
    let rest = extent.saturating_sub(used);
    let weights: Vec<u64> = sizes
        .iter()
        .map(|s| match s {
            Size::Weight(w) => u64::from((*w).max(1)),
            Size::Fixed(_) | Size::Auto => 0,
        })
        .collect();
    let floor = |i: usize| u64::from(suelos.get(i).copied().unwrap_or(0));
    let floors_fit = (0..sizes.len())
        .filter(|i| weights[*i] > 0)
        .map(floor)
        .sum::<u64>()
        <= rest;
    // Weighted ones that already get their floor leave the proportional layout.
    let mut at_floor = vec![false; sizes.len()];
    loop {
        let fixed: u64 = (0..sizes.len()).filter(|i| at_floor[*i]).map(floor).sum();
        let free = rest.saturating_sub(fixed);
        let active: Vec<usize> = (0..sizes.len())
            .filter(|i| weights[*i] > 0 && !at_floor[*i])
            .collect();
        let total: u64 = active.iter().map(|i| weights[*i]).sum();
        let mut given: u64 = 0;
        for (n, &i) in active.iter().enumerate() {
            assigned[i] = if n + 1 == active.len() {
                free.saturating_sub(given)
            } else {
                free.saturating_mul(weights[i])
                    .checked_div(total)
                    .unwrap_or(0)
            };
            given = given.saturating_add(assigned[i]);
        }
        for i in 0..sizes.len() {
            if at_floor[i] {
                assigned[i] = floor(i);
            }
        }
        if !floors_fit {
            break;
        }
        let below: Vec<usize> = active
            .iter()
            .copied()
            .filter(|i| assigned[*i] < floor(*i))
            .collect();
        // Each round fixes at least one more, so it terminates.
        if below.is_empty() {
            break;
        }
        for i in below {
            at_floor[i] = true;
        }
    }
    let mut out = Vec::with_capacity(sizes.len());
    let mut off: u64 = 0;
    for size in assigned {
        let o = u16::try_from(off).unwrap_or(u16::MAX);
        let s16 = u16::try_from(size).unwrap_or(u16::MAX);
        out.push(match dir {
            Dir::Horizontal => Rect::new(area.x.saturating_add(o), area.y, s16, area.height),
            Dir::Vertical => Rect::new(area.x, area.y.saturating_add(o), area.width, s16),
        });
        off = off.saturating_add(size);
    }
    out
}

/// Places `node` in `area`, accumulating into `out`.
fn place(node: &Node, area: Rect, decls: &KindRegistry, out: &mut Resolved) {
    match node {
        Node::Slot { id, kind, .. } => {
            out.placements.push((*id, area));
            if decls.get(kind).is_some_and(|d| d.focusable) {
                out.focus_order.push(*id);
            }
        }
        Node::Tabs { children, active } => {
            let idx = if *active < children.len() {
                *active
            } else {
                out.diagnostics.push(LayoutDiagnostic::ActiveClamped {
                    was: *active,
                    to: 0,
                });
                0
            };
            solo_one(children, idx, area, decls, out);
        }
        Node::Split {
            dir,
            children,
            sizes,
        } => {
            let sizes_out: Vec<Size> = (0..children.len())
                .map(|i| match sizes.get(i) {
                    Some(Size::Weight(0)) => {
                        out.diagnostics
                            .push(LayoutDiagnostic::ZeroWeightRaised { at: i });
                        Size::Weight(1)
                    }
                    Some(other) => *other,
                    None => Size::Weight(1),
                })
                .collect();
            let floors: Vec<u16> = children
                .iter()
                .map(|c| {
                    let (mw, mh) = min_of(c, decls);
                    match dir {
                        Dir::Horizontal => mw,
                        Dir::Vertical => mh,
                    }
                })
                .collect();
            let rects = distribute(area, *dir, &sizes_out, &floors);
            // A `Split` collapses ONLY if all its children are weighted.
            //
            // Collapsing is "these siblings compete for the same axis and
            // do not fit, so show one". As soon as there is a fixed child,
            // the layout is no longer a dispute: it is docked chrome plus a
            // flexible zone, and collapsing it would take the chrome down
            // with it. With the `orthodox` preset that would literally mean
            // losing the status bar on a short terminal. What DOES collapse
            // is the flexible zone on its own, when its turn comes.
            let fits = !sizes_out.iter().all(|s| matches!(s, Size::Weight(_)))
                || children.iter().zip(&rects).all(|(c, r)| {
                    let (mw, mh) = min_of(c, decls);
                    r.width >= mw && r.height >= mh
                });
            if fits {
                for (c, r) in children.iter().zip(rects) {
                    place(c, r, decls, out);
                }
            } else {
                // Degraded to tabs FOR THIS FRAME. If the remaining child
                // does not fit either, it will collapse in turn: that is
                // how it propagates upward without anyone having to count
                // levels.
                solo_one(children, 0, area, decls, out);
            }
        }
    }
}

/// Places child `idx` over the whole area and sends the rest's slots to
/// `hidden` — which is the suspension signal, not a painting detail.
fn solo_one(children: &[Node], idx: usize, area: Rect, decls: &KindRegistry, out: &mut Resolved) {
    for (i, c) in children.iter().enumerate() {
        if i == idx {
            place(c, area, decls, out);
        } else {
            out.hidden.extend(c.slot_ids());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{KindId, LayoutDiagnostic};
    use proptest::prelude::*;

    fn reg() -> KindRegistry {
        KindRegistry::builtin()
    }
    fn r(x: u16, y: u16, w: u16, h: u16) -> Rect {
        Rect::new(x, y, w, h)
    }
    fn two(a: Node, b: Node, dir: Dir) -> Node {
        Node::Split {
            dir,
            sizes: vec![Size::Weight(1), Size::Weight(1)],
            children: vec![a, b],
        }
    }
    fn browser(id: u32) -> Node {
        Node::slot(SlotId(id), KindId::browser())
    }

    /// The `orthodox` case: two browsers at 50%. It is today's screen
    /// expressed in the new model, and being expressible is the proof that
    /// the model is well built.
    #[test]
    fn two_browsers_at_fifty_percent() {
        let tree = two(browser(1), browser(2), Dir::Horizontal);
        let out = resolve(r(0, 0, 100, 30), &tree, &reg());
        assert_eq!(
            out.placements,
            vec![(SlotId(1), r(0, 0, 50, 30)), (SlotId(2), r(50, 0, 50, 30))]
        );
        assert!(out.hidden.is_empty());
        assert_eq!(out.focus_order, vec![SlotId(1), SlotId(2)]);
    }

    /// REGRESSION (2026-09-21 capture): a WEIGHT does not go below its
    /// kind's minimum while the free room is enough for all of them. The
    /// saved layout had listings at 49/51 and the viewer at 1, next to
    /// fixed ones: the viewer came out one pixel short, and a `Split` with
    /// fixed ones does not collapse. Now it takes its minimum (20) and the
    /// other weights give up the difference in proportion.
    #[test]
    fn a_weight_does_not_go_below_its_minimum_if_there_is_room() {
        let tree = Node::Split {
            dir: Dir::Horizontal,
            children: vec![
                Node::slot(SlotId(5), KindId::new("places")),
                browser(1),
                browser(2),
                Node::slot(SlotId(9), KindId::new("viewer")),
            ],
            sizes: vec![
                Size::Fixed(16),
                Size::Weight(49),
                Size::Weight(51),
                Size::Weight(1),
            ],
        };
        let out = resolve(r(0, 0, 200, 30), &tree, &reg());
        let width = |id: u32| {
            out.placements
                .iter()
                .find(|(s, _)| *s == SlotId(id))
                .map(|(_, re)| re.width)
                .expect("placed")
        };
        assert_eq!(width(9), 20, "the viewer, at its minimum");
        assert_eq!(width(5), 16, "the fixed one does not give way");
        assert_eq!(width(1) + width(2) + 20 + 16, 200, "nothing is lost");
        assert!(width(2) > width(1), "the others keep their proportion");
        // With no room for all the minimums, layout is as usual.
        let out = resolve(r(0, 0, 60, 30), &tree, &reg());
        assert!(out.placements.len() + out.hidden.len() >= 4);
    }

    /// An odd width cannot lose a column: the remainder goes to the last one.
    #[test]
    fn an_odd_width_does_not_lose_a_column() {
        let tree = two(browser(1), browser(2), Dir::Horizontal);
        let out = resolve(r(0, 0, 101, 30), &tree, &reg());
        let width: u16 = out.placements.iter().map(|(_, re)| re.width).sum();
        assert_eq!(width, 101, "a column was lost in the layout");
    }

    /// A VERTICAL cut lays out the height, and the `y`s chain up.
    #[test]
    fn a_vertical_cut_divides_the_height() {
        let tree = two(browser(1), browser(2), Dir::Vertical);
        let out = resolve(r(0, 0, 40, 20), &tree, &reg());
        assert_eq!(
            out.placements,
            vec![(SlotId(1), r(0, 0, 40, 10)), (SlotId(2), r(0, 10, 40, 10))]
        );
    }

    /// Only the ACTIVE tab is placed; the others go to `hidden`, which is
    /// the suspension signal (out go watches, probes and plugin columns).
    #[test]
    fn an_inactive_tab_goes_to_hidden_not_to_placements() {
        let tree = Node::Tabs {
            active: 1,
            children: vec![browser(1), browser(2)],
        };
        let out = resolve(r(0, 0, 100, 30), &tree, &reg());
        assert_eq!(out.placements, vec![(SlotId(2), r(0, 0, 100, 30))]);
        assert_eq!(out.hidden, vec![SlotId(1)]);
        assert_eq!(
            out.focus_order,
            vec![SlotId(2)],
            "tabbing does not go through what is not seen"
        );
    }

    /// Asking BEFORE splitting: in 30 columns two browsers of minimum 20 do
    /// not fit, so splitting there only produces a panel layout hides in
    /// the same frame.
    ///
    /// The same count the collapse does — the KIND's minimum, not the
    /// content one — so the two answers cannot drift apart.
    #[test]
    fn has_room_to_split_says_no_when_the_split_would_collapse() {
        let k = KindId::browser();
        assert!(has_room_to_split(
            r(0, 0, 40, 30),
            Dir::Horizontal,
            &k,
            &reg()
        ));
        assert!(!has_room_to_split(
            r(0, 0, 30, 30),
            Dir::Horizontal,
            &k,
            &reg()
        ));
        // The axis being split is what counts: 30 columns are not enough
        // for two sideways and the same 30 rows are enough for two
        // vertically.
        assert!(has_room_to_split(
            r(0, 0, 30, 30),
            Dir::Vertical,
            &k,
            &reg()
        ));
        assert!(!has_room_to_split(
            r(0, 0, 30, 9),
            Dir::Vertical,
            &k,
            &reg()
        ));
    }

    /// ADR 0138: stacking two listings where they do not fit hides one, and
    /// that is refused; with room, it is not. Grouping them into tabs
    /// leaves one visible, and with two visible before, that is also
    /// refused.
    #[test]
    fn moving_cannot_leave_anything_outside_the_view() {
        use crate::layout::DropZone;
        let b = |id| Node::slot(SlotId(id), KindId::browser());
        let tree = Node::split(Dir::Horizontal, vec![b(1), b(2)]);
        let apilado = tree.move_slot(SlotId(1), SlotId(2), DropZone::Bottom);
        let together = tree.move_slot(SlotId(1), SlotId(2), DropZone::Center);
        let below = r(0, 0, 100, 8);
        let alto = r(0, 0, 100, 40);
        let ok = |area, new: &Node, tolerado| {
            keeps_on_screen(
                &resolve(area, &tree, &reg()),
                &resolve(area, new, &reg()),
                new,
                tolerado,
            )
        };
        assert!(!ok(below, &apilado, None), "at 8 rows one gets hidden");
        assert!(ok(alto, &apilado, None));
        assert!(!ok(alto, &together, Some(SlotId(2))), "only one visible");
    }

    /// And what it says matches what layout does: if it says yes, both
    /// slots are placed; if it says no, the `Split` collapses.
    #[test]
    fn has_room_to_split_matches_the_collapse() {
        let k = KindId::browser();
        for (w, h, dir) in [
            (40, 30, Dir::Horizontal),
            (30, 30, Dir::Horizontal),
            (30, 30, Dir::Vertical),
            (30, 9, Dir::Vertical),
        ] {
            let area = r(0, 0, w, h);
            let tree = two(browser(1), browser(2), dir);
            let placed = resolve(area, &tree, &reg()).placements.len();
            assert_eq!(
                has_room_to_split(area, dir, &k, &reg()),
                placed == 2,
                "{w}x{h} {dir:?}: placed {placed}"
            );
        }
    }

    /// The collapse: two browsers of minimum 20 do not fit in 30 columns, so
    /// the `Split` degrades to `Tabs` FOR THIS FRAME. The tree is not touched.
    #[test]
    fn a_split_that_does_not_fit_collapses_to_tabs_without_touching_the_tree() {
        let tree = two(browser(1), browser(2), Dir::Horizontal);
        let before = tree.clone();
        let out = resolve(r(0, 0, 30, 30), &tree, &reg());
        assert_eq!(out.placements.len(), 1, "only one fits");
        assert_eq!(out.hidden, vec![SlotId(2)]);
        assert_eq!(tree, before, "resolve CANNOT mutate the tree");
    }

    /// The collapse PROPAGATES: if after degrading one level it still does
    /// not fit, it degrades the one above. Without this, a very narrow
    /// window paints two-column boxes instead of a usable screen.
    #[test]
    fn the_collapse_propagates_upward() {
        let tree = two(
            two(browser(1), browser(2), Dir::Horizontal),
            browser(3),
            Dir::Horizontal,
        );
        let out = resolve(r(0, 0, 30, 30), &tree, &reg());
        assert_eq!(out.placements.len(), 1);
        assert_eq!(out.placements[0].1, r(0, 0, 30, 30), "takes up everything");
    }

    /// NOTHING fits: it is painted anyway, breaking the minimum. Never a
    /// blank screen — a user with a tiny terminal sees something and a
    /// message, not an emptiness that looks like a hang.
    #[test]
    fn when_nothing_fits_one_is_painted_anyway() {
        let tree = browser(1);
        let out = resolve(r(0, 0, 6, 2), &tree, &reg());
        assert_eq!(out.placements, vec![(SlotId(1), r(0, 0, 6, 2))]);
        assert!(out.hidden.is_empty());
    }

    /// A kind outside the registry is placed the same way (a box with its
    /// name) but does not enter the focus order: tabbing does not go to
    /// something nobody knows how to paint.
    #[test]
    fn an_unknown_kind_is_placed_but_does_not_take_focus() {
        // A name that will NEVER be registered: this used to say
        // `terminal`, and the day the terminal panel entered the registry
        // this test would have stopped testing an unknown kind without
        // saying so.
        let tree = Node::slot(SlotId(9), KindId::new("un-kind-que-no-existe"));
        let out = resolve(r(0, 0, 100, 30), &tree, &reg());
        assert_eq!(out.placements.len(), 1);
        assert!(out.focus_order.is_empty());
    }

    /// `tasks` se coloca pero no toma foco: se mira, no se enfoca.
    #[test]
    fn tasks_is_painted_but_not_tabbed_to() {
        let tree = two(
            browser(1),
            Node::slot(SlotId(2), KindId::new("tasks")),
            Dir::Vertical,
        );
        let out = resolve(r(0, 0, 100, 30), &tree, &reg());
        assert_eq!(out.placements.len(), 2);
        assert_eq!(out.focus_order, vec![SlotId(1)]);
    }

    /// `active` fuera de rango se clampa y se CUENTA; no es un error duro.
    #[test]
    fn an_out_of_range_active_is_clamped_with_a_diagnostic() {
        let tree = Node::Tabs {
            active: 7,
            children: vec![browser(1)],
        };
        let out = resolve(r(0, 0, 100, 30), &tree, &reg());
        assert_eq!(out.placements.len(), 1);
        assert!(
            out.diagnostics
                .iter()
                .any(|d| matches!(d, LayoutDiagnostic::ActiveClamped { .. }))
        );
    }

    /// Un peso de cero no reparte nada: se sube a uno y se cuenta.
    #[test]
    fn a_weight_of_zero_is_raised_to_one_with_a_diagnostic() {
        let tree = Node::Split {
            dir: Dir::Horizontal,
            sizes: vec![Size::Weight(0), Size::Weight(1)],
            children: vec![browser(1), browser(2)],
        };
        let out = resolve(r(0, 0, 100, 30), &tree, &reg());
        assert_eq!(
            out.placements.len(),
            2,
            "the zero-weight one is also painted"
        );
        assert!(
            out.diagnostics
                .iter()
                .any(|d| matches!(d, LayoutDiagnostic::ZeroWeightRaised { .. }))
        );
    }

    /// A `Fixed` child charges what is its and `Weight`s share the rest.
    #[test]
    fn a_fixed_one_takes_its_share_and_the_weighted_ones_take_the_rest() {
        let tree = Node::Split {
            dir: Dir::Vertical,
            sizes: vec![Size::Weight(1), Size::Fixed(4), Size::Weight(1)],
            children: vec![browser(1), browser(2), browser(3)],
        };
        let out = resolve(r(0, 0, 40, 24), &tree, &reg());
        let heights: Vec<u16> = out.placements.iter().map(|(_, re)| re.height).collect();
        assert_eq!(
            heights,
            vec![10, 4, 10],
            "20 remaining between two weights, and the fixed one apart"
        );
    }

    /// A `Fixed` below its kind's minimum IS RESPECTED: the minimum decides
    /// when a proportional layout collapses, it does not override an
    /// explicit order.
    #[test]
    fn a_fixed_size_below_its_kinds_minimum_is_respected() {
        let tree = Node::Split {
            dir: Dir::Vertical,
            sizes: vec![Size::Weight(1), Size::Fixed(1)],
            children: vec![browser(1), browser(2)],
        };
        let out = resolve(r(0, 0, 40, 24), &tree, &reg());
        assert_eq!(
            out.placements.len(),
            2,
            "does not collapse because of the fixed one"
        );
        assert_eq!(
            out.placements[1].1.height, 1,
            "one row, even though the minimum is 5"
        );
    }

    /// If the fixed ones no longer fit they are clipped in order and the
    /// weighted ones are left with nothing. It is what makes a tiny
    /// terminal keep painting the status bar instead of nothing.
    #[test]
    fn if_the_fixed_ones_do_not_fit_they_are_trimmed_and_the_weighted_ones_get_nothing() {
        let tree = Node::Split {
            dir: Dir::Vertical,
            sizes: vec![Size::Fixed(3), Size::Fixed(9), Size::Weight(1)],
            children: vec![browser(1), browser(2), browser(3)],
        };
        let out = resolve(r(0, 0, 40, 5), &tree, &reg());
        let heights: Vec<u16> = out.placements.iter().map(|(_, re)| re.height).collect();
        assert_eq!(
            heights,
            vec![3, 2, 0],
            "the first whole, the second clipped, the weight to zero"
        );
    }

    /// A `Split` with a fixed child does NOT collapse even if the weighted
    /// one is left with no room: collapsing it would take the chrome down
    /// with it. With `orthodox` that would mean losing the status bar on a
    /// short terminal.
    #[test]
    fn a_split_with_a_fixed_child_does_not_collapse() {
        let tree = Node::Split {
            dir: Dir::Vertical,
            sizes: vec![Size::Weight(1), Size::Fixed(1)],
            children: vec![browser(1), Node::slot(SlotId(2), KindId::new("status"))],
        };
        let out = resolve(r(0, 0, 40, 2), &tree, &reg());
        assert_eq!(out.placements.len(), 2, "the status bar survives");
        assert_eq!(out.placements[1].1.height, 1);
    }

    /// **#229**: docked chrome is SET ASIDE before leaving the screen with
    /// no usable listing.
    ///
    /// It is the `full` preset at 40x10: a fixed 16-wide sidebar and a
    /// fixed 30-wide right column already exceed 40 columns, so both
    /// browsers charged ZERO and disappeared; and below, processes (8) plus
    /// the bar (1) left the main row at a single line. What remained on
    /// screen was three chrome headers and not one file name.
    #[test]
    fn the_chrome_is_removed_before_leaving_the_screen_without_a_listing() {
        let derecha = Node::Split {
            dir: Dir::Vertical,
            sizes: vec![Size::Weight(1), Size::Weight(1)],
            children: vec![
                Node::slot(SlotId(6), KindId::new("viewer")),
                Node::slot(SlotId(8), KindId::new("metadata")),
            ],
        };
        let row = Node::Split {
            dir: Dir::Horizontal,
            sizes: vec![
                Size::Fixed(16),
                Size::Weight(1),
                Size::Weight(1),
                Size::Fixed(30),
            ],
            children: vec![
                Node::slot(SlotId(5), KindId::new("places")),
                browser(1),
                browser(2),
                derecha,
            ],
        };
        let tree = Node::Split {
            dir: Dir::Vertical,
            sizes: vec![Size::Weight(1), Size::Fixed(8), Size::Fixed(1)],
            children: vec![
                row,
                Node::slot(SlotId(7), KindId::new("processes")),
                Node::slot(SlotId(4), KindId::new("status")),
            ],
        };
        let out = resolve(r(0, 0, 40, 10), &tree, &reg());

        let listings: Vec<&(SlotId, Rect)> = out
            .placements
            .iter()
            .filter(|(id, _)| *id == SlotId(1) || *id == SlotId(2))
            .collect();
        assert!(
            listings
                .iter()
                .any(|(_, re)| re.width >= 4 && re.height >= 4),
            "no listing shows a row: {:?}",
            out.placements
        );
        // What gets set aside is the LARGE one on a short axis, and nothing
        // else: the right column (30 wide) and the processes panel (8
        // tall). What fit stays — the sidebar and the status bar — which
        // is the difference between "this layout adapts" and "this layout
        // gives up".
        for id in [SlotId(6), SlotId(8), SlotId(7)] {
            assert!(out.hidden.contains(&id), "{id:?} should be set aside");
        }
        for id in [SlotId(5), SlotId(4)] {
            assert!(
                out.placements.iter().any(|(p, _)| *p == id),
                "{id:?} fit and is gone: {:?}",
                out.placements
            );
        }
        // And setting aside means SUSPENDING, with its diagnostic: without
        // the first, an invisible panel is left with its watch open, and
        // without the second nobody knows why their sidebar is not there.
        assert!(
            out.diagnostics
                .iter()
                .any(|d| matches!(d, LayoutDiagnostic::ChromeSetAside { .. })),
            "no diagnostic: {:?}",
            out.diagnostics
        );
    }

    /// Two listings that each fail on A DIFFERENT AXIS are still a screen
    /// with no usable listing.
    ///
    /// Asking "is any wide enough?" and "is any tall enough?" separately
    /// answered that no axis was missing — one satisfies each question —
    /// and the rescue was never attempted. It is measured over the BEST
    /// candidate, which is what the screen being usable depends on.
    #[test]
    fn two_lame_listings_of_different_axes_do_not_make_a_good_screen() {
        let tree = Node::Split {
            dir: Dir::Vertical,
            sizes: vec![Size::Weight(1), Size::Fixed(9)],
            children: vec![
                browser(1),
                Node::Split {
                    dir: Dir::Horizontal,
                    sizes: vec![Size::Fixed(30), Size::Weight(1)],
                    children: vec![Node::slot(SlotId(3), KindId::new("viewer")), browser(2)],
                },
            ],
        };
        let out = resolve(r(0, 0, 32, 10), &tree, &reg());
        assert!(
            out.placements
                .iter()
                .any(|(id, re)| (*id == SlotId(1) || *id == SlotId(2))
                    && re.width >= 12
                    && re.height >= 4),
            "neither of the two listings ended up usable: {:?}",
            out.placements
        );
    }

    /// Setting aside a tab BEFORE another does not change which one is
    /// being looked at.
    ///
    /// The active index is a position, so removing the one before it
    /// shifted all the ones after: the next one was painted and the one
    /// the user had open was suspended.
    #[test]
    fn detaching_a_tab_does_not_change_the_one_being_viewed() {
        let tree = Node::Split {
            dir: Dir::Horizontal,
            sizes: vec![Size::Fixed(30), Size::Weight(1)],
            children: vec![
                Node::slot(SlotId(9), KindId::new("viewer")),
                Node::Tabs {
                    active: 1,
                    children: vec![browser(1), browser(2)],
                },
            ],
        };
        let out = resolve(r(0, 0, 34, 10), &tree, &reg());
        assert!(
            out.placements.iter().any(|(id, _)| *id == SlotId(2)),
            "tab 2 is being looked at, and it is the one that has to remain: {:?}",
            out.placements
        );
        assert!(out.hidden.contains(&SlotId(1)));
    }

    /// A DEMANDING kind does not drag the rescue into setting chrome aside
    /// chasing a size the screen does not have: the floor is its CAPPED
    /// minimum.
    #[test]
    fn a_demanding_kind_does_not_empty_the_screen_of_chrome() {
        let mut reg = reg();
        reg.insert(crate::layout::KindDecl {
            id: KindId::new("exigente"),
            min: (80, 30),
            focusable: true,
            takes_keys: true,
            multi: false,
            roles: &[RoleId::Active],
        });
        let tree = Node::Split {
            dir: Dir::Horizontal,
            sizes: vec![Size::Fixed(10), Size::Weight(1)],
            children: vec![
                Node::slot(SlotId(5), KindId::new("places")),
                Node::slot(SlotId(1), KindId::new("exigente")),
            ],
        };
        let out = resolve(r(0, 0, 40, 10), &tree, &reg);
        assert!(
            out.placements.iter().any(|(id, _)| *id == SlotId(5)),
            "the sidebar fit: 30 columns are enough to show something"
        );
    }

    /// A tree with the SAME id twice does not lose a copy when chrome is
    /// set aside.
    ///
    /// [`super::validate`] rejects repeated ids, but `resolve` has to
    /// withstand any tree, and this rule's first version set aside "the
    /// slots with these ids": it also took the other copy with it, left
    /// unpainted AND unsuspended — an open watch staring at nothing. The
    /// partition property caught it; the minimal case is pinned in
    /// `proptest-regressions/layout/resolve.txt` and this names it.
    #[test]
    fn with_repeated_ids_detaching_chrome_does_not_lose_a_slot() {
        let tree = Node::Split {
            dir: Dir::Vertical,
            sizes: vec![Size::Weight(1), Size::Weight(1)],
            children: vec![
                Node::Split {
                    dir: Dir::Vertical,
                    sizes: vec![Size::Fixed(2)],
                    children: vec![Node::slot(SlotId(10), KindId::new("tasks"))],
                },
                Node::Tabs {
                    active: 1,
                    children: vec![browser(1), Node::slot(SlotId(10), KindId::browser())],
                },
            ],
        };
        let out = resolve(r(0, 0, 4, 4), &tree, &reg());
        let mut seen: Vec<SlotId> = out.placements.iter().map(|(id, _)| *id).collect();
        seen.extend(out.hidden.iter().copied());
        seen.sort_unstable();
        let mut all = tree.slot_ids();
        all.sort_unstable();
        assert_eq!(seen, all, "a slot was left unpainted and unsuspended");
    }

    /// When only HEIGHT is missing, the width chrome is not touched. It is
    /// the same `full` at 80x10: there is width to spare for the sidebar
    /// and the right column, and the only thing in the way is the
    /// processes panel's eight rows.
    #[test]
    fn only_the_chrome_of_the_missing_axis_is_set_aside() {
        let derecha = Node::Split {
            dir: Dir::Vertical,
            sizes: vec![Size::Weight(1), Size::Weight(1)],
            children: vec![
                Node::slot(SlotId(6), KindId::new("viewer")),
                Node::slot(SlotId(8), KindId::new("metadata")),
            ],
        };
        let row = Node::Split {
            dir: Dir::Horizontal,
            sizes: vec![
                Size::Fixed(16),
                Size::Weight(1),
                Size::Weight(1),
                Size::Fixed(30),
            ],
            children: vec![
                Node::slot(SlotId(5), KindId::new("places")),
                browser(1),
                browser(2),
                derecha,
            ],
        };
        let tree = Node::Split {
            dir: Dir::Vertical,
            sizes: vec![Size::Weight(1), Size::Fixed(8), Size::Fixed(1)],
            children: vec![
                row,
                Node::slot(SlotId(7), KindId::new("processes")),
                Node::slot(SlotId(4), KindId::new("status")),
            ],
        };
        let out = resolve(r(0, 0, 80, 10), &tree, &reg());
        // Set aside: ONLY the processes panel, which is the only chrome on
        // the missing axis. The sidebar and the right column are WIDTH
        // chrome and there is room to spare there, so they are not touched.
        assert!(out.hidden.contains(&SlotId(7)));
        for id in [SlotId(5), SlotId(1), SlotId(2), SlotId(6), SlotId(4)] {
            assert!(
                out.placements.iter().any(|(p, _)| *p == id),
                "{id:?} should still be on screen: {:?}",
                out.placements
            );
        }
        // And the attribute sheet is ALSO painted: in the nine remaining
        // rows a viewer (minimum 5) and a sheet (minimum 4) just fit.
        // Before, the proportional layout gave 4/5, the viewer fell below
        // its minimum and the `Split` degraded to tabs hiding the sheet;
        // with the weights' floor each gets its minimum.
        assert!(
            out.placements.iter().any(|(p, _)| *p == SlotId(8)),
            "{:?}",
            out.placements
        );
        assert_eq!(
            out.diagnostics
                .iter()
                .filter(|d| matches!(d, LayoutDiagnostic::ChromeSetAside { .. }))
                .count(),
            1,
            "a single panel set aside: {:?}",
            out.diagnostics
        );
    }

    /// Setting aside chrome that fixes NOTHING is not done: at 40x2 no
    /// listing reaches its minimum even removing the bar, so the bar
    /// stays. It is the other half of #229, and what stops a tiny terminal
    /// from losing chrome for nothing in return.
    #[test]
    fn the_chrome_is_not_removed_if_removing_it_fixes_nothing() {
        let tree = Node::Split {
            dir: Dir::Vertical,
            sizes: vec![Size::Weight(1), Size::Fixed(1)],
            children: vec![browser(1), Node::slot(SlotId(2), KindId::new("status"))],
        };
        let out = resolve(r(0, 0, 40, 2), &tree, &reg());
        assert_eq!(out.placements.len(), 2, "the status bar survives");
        assert!(out.hidden.is_empty());
    }

    /// A FIXED child that is itself a listing is not chrome: setting it
    /// aside would be taking away one screen to give it to another. The
    /// table in
    /// [`if_the_fixed_ones_do_not_fit_they_are_trimmed_and_the_weighted_ones_get_nothing`]
    /// still holds as is, and this names it.
    #[test]
    fn a_fixed_slot_that_is_a_listing_is_not_chrome() {
        let tree = Node::Split {
            dir: Dir::Vertical,
            sizes: vec![Size::Fixed(3), Size::Fixed(9), Size::Weight(1)],
            children: vec![browser(1), browser(2), browser(3)],
        };
        let out = resolve(r(0, 0, 40, 5), &tree, &reg());
        let heights: Vec<u16> = out.placements.iter().map(|(_, re)| re.height).collect();
        assert_eq!(
            heights,
            vec![3, 2, 0],
            "nothing to set aside, nothing changes"
        );
        assert!(out.hidden.is_empty());
    }

    /// `Auto` counts as zero if it reaches this far. It should not —
    /// `substitute_auto` replaces it — but a layout pass is no place to
    /// blow up.
    #[test]
    fn an_unsubstituted_auto_counts_as_zero() {
        let tree = Node::Split {
            dir: Dir::Vertical,
            sizes: vec![Size::Weight(1), Size::Auto],
            children: vec![browser(1), browser(2)],
        };
        let out = resolve(r(0, 0, 40, 24), &tree, &reg());
        let heights: Vec<u16> = out.placements.iter().map(|(_, re)| re.height).collect();
        assert_eq!(heights, vec![24, 0]);
    }

    // --- properties ---

    fn they_overlap(a: Rect, b: Rect) -> bool {
        a.x < b.x + b.width && b.x < a.x + a.width && a.y < b.y + b.height && b.y < a.y + a.height
    }

    /// Arbitrary sizes, `Auto` included: `resolve` should never see it —
    /// `substitute_auto` replaces it — and the properties have to withstand
    /// someone skipping that step.
    fn tam_arbitrario() -> impl Strategy<Value = Size> {
        prop_oneof![
            (0u16..5).prop_map(Size::Weight),
            (0u16..12).prop_map(Size::Fixed),
            Just(Size::Auto),
        ]
    }

    /// Like [`tree_arbitrario`] but with no `Fixed` or `Auto`.
    fn tree_ponderado() -> impl Strategy<Value = Node> {
        let kinds = prop::sample::select(vec![
            "browser", "tasks", "viewer", "compare", "sync", "terminal",
        ]);
        let sheet = (0u32..64, kinds).prop_map(|(id, k)| Node::slot(SlotId(id), KindId::new(k)));
        sheet.prop_recursive(3, 24, 4, |inner| {
            prop_oneof![
                (prop::collection::vec(inner.clone(), 1..4), any::<bool>()).prop_map(
                    |(children, horiz)| Node::split(
                        if horiz {
                            Dir::Horizontal
                        } else {
                            Dir::Vertical
                        },
                        children
                    )
                ),
                (prop::collection::vec(inner, 1..4), 0usize..5)
                    .prop_map(|(children, active)| Node::Tabs { children, active }),
            ]
        })
    }

    fn tree_arbitrario() -> impl Strategy<Value = Node> {
        let kinds = prop::sample::select(vec![
            "browser", "tasks", "viewer", "compare", "sync", "terminal",
        ]);
        let sheet = (0u32..64, kinds).prop_map(|(id, k)| Node::slot(SlotId(id), KindId::new(k)));
        sheet.prop_recursive(3, 24, 4, |inner| {
            prop_oneof![
                (
                    prop::collection::vec(inner.clone(), 1..4),
                    prop::collection::vec(tam_arbitrario(), 1..4),
                    any::<bool>()
                )
                    .prop_map(|(children, mut sizes, horiz)| {
                        sizes.resize(children.len(), Size::Weight(1));
                        Node::Split {
                            dir: if horiz {
                                Dir::Horizontal
                            } else {
                                Dir::Vertical
                            },
                            children,
                            sizes,
                        }
                    }),
                (prop::collection::vec(inner, 1..4), 0usize..5)
                    .prop_map(|(children, active)| Node::Tabs { children, active }),
            ]
        })
    }

    proptest! {
        /// Placements NEVER overlap and never spill outside the area. In a
        /// TUI an overlap does not look like a layout bug: it looks like
        /// corrupted text, and gets chased in the wrong place.
        #[test]
        fn placements_neither_overlap_nor_spill_out(
            tree in tree_arbitrario(), w in 1u16..200, h in 1u16..80
        ) {
            let out = resolve(Rect::new(0, 0, w, h), &tree, &reg());
            for (i, (_, a)) in out.placements.iter().enumerate() {
                prop_assert!(a.x + a.width <= w && a.y + a.height <= h);
                for (_, b) in out.placements.iter().skip(i + 1) {
                    prop_assert!(!they_overlap(*a, *b), "{a:?} overlaps {b:?}");
                }
            }
        }

        /// Every placed slot meets its minimum, except in the "nothing
        /// fits" case, recognized because there is only ONE placement.
        ///
        /// Over ONLY-WEIGHTED trees, because the property belongs to the
        /// proportional layout: a `Fixed` child can end up below its
        /// minimum on purpose — the user asked for it — and that has its
        /// own table tests.
        #[test]
        fn everything_placed_meets_its_minimum(
            tree in tree_ponderado(), w in 1u16..200, h in 1u16..80
        ) {
            // With repeated ids `kind_of` returns the FIRST one's, so the
            // minimum we would compare against might not be this slot's.
            prop_assume!(tree.duplicate_slot_ids().is_empty());
            let out = resolve(Rect::new(0, 0, w, h), &tree, &reg());
            if out.placements.len() > 1 {
                for (id, a) in &out.placements {
                    let kind = tree.kind_of(*id).expect("placed so it is there");
                    let (mw, mh) = reg().min_of(kind);
                    prop_assert!(a.width >= mw && a.height >= mh, "{id:?} {a:?} < ({mw},{mh})");
                }
            }
        }

        /// `placements` and `hidden` PARTITION the tree's slots. If this
        /// fails, a live slot is left unpainted AND unsuspended — with its
        /// watch open and nobody watching it.
        #[test]
        fn placements_and_hidden_partition_the_tree(
            tree in tree_arbitrario(), w in 1u16..200, h in 1u16..80
        ) {
            let out = resolve(Rect::new(0, 0, w, h), &tree, &reg());
            let mut seen: Vec<SlotId> = out.placements.iter().map(|(id, _)| *id).collect();
            seen.extend(out.hidden.iter().copied());
            seen.sort_unstable();
            let mut all = tree.slot_ids();
            all.sort_unstable();
            prop_assert_eq!(seen, all);
        }

        /// `focus_order` only carries placed, focusable ones, with no repeats.
        #[test]
        fn the_focus_order_only_carries_visible_focusable_ones(
            tree in tree_arbitrario(), w in 1u16..200, h in 1u16..80
        ) {
            prop_assume!(tree.duplicate_slot_ids().is_empty());
            let out = resolve(Rect::new(0, 0, w, h), &tree, &reg());
            let placed: Vec<SlotId> = out.placements.iter().map(|(id, _)| *id).collect();
            for id in &out.focus_order {
                prop_assert!(placed.contains(id));
                let kind = tree.kind_of(*id).expect("focusable so it is there");
                prop_assert!(reg().get(kind).is_some_and(|d| d.focusable));
            }
        }

        /// Never an empty screen: if there is a slot, one is painted.
        #[test]
        fn something_is_always_painted(
            tree in tree_arbitrario(), w in 1u16..200, h in 1u16..80
        ) {
            let out = resolve(Rect::new(0, 0, w, h), &tree, &reg());
            prop_assert!(!out.placements.is_empty());
        }
    }
}
