//! The tree: three node types and the types that identify them.
//!
//! It is the ONE format — the layout file, L2's session blob and whatever
//! the layout editor will spit out are this same thing. Two formats would
//! force migrating between them, which is exactly what ADR 0058 avoids.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// A rectangle in CELLS.
///
/// Its own and not ratatui's: this crate depends on no toolkit, and the GUI
/// scales these cells by its font metric. The fields are named the same as
/// `ratatui::layout::Rect`'s on purpose, so the conversion in the TUI is
/// field for field with no interpreting to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rect {
    /// Column of the top-left corner.
    pub x: u16,
    /// Row of the top-left corner.
    pub y: u16,
    /// Width in cells.
    pub width: u16,
    /// Height in cells.
    pub height: u16,
}

impl Rect {
    /// Construction shorthand, heavily used by the tests and by layout.
    #[must_use]
    pub const fn new(x: u16, y: u16, width: u16, height: u16) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }
}

/// A slot's identity.
///
/// Minted per layout and NOT reused within a session: closing a slot leaves
/// its state orphaned in [`crate::layout::SlotStore`], so reopening the
/// same layout recovers the history instead of starting blank.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SlotId(pub u32);

/// What is inside a slot.
///
/// A STRING and not an enum: an enum closes the registry, and with it the
/// door to a plugin contributing a kind (ADR 0058 D2). A kind this binary
/// does not know is not an error — it is painted as a box with its name and
/// its `params` are kept when reserialized.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct KindId(String);

impl KindId {
    /// Any kind, by name.
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// The file listing's kind.
    #[must_use]
    pub fn browser() -> Self {
        Self::new("browser")
    }

    /// The name, for each frontend's renderer table.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A [`Node::Split`]'s direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Dir {
    /// The children split the WIDTH, one next to the other.
    Horizontal,
    /// The children split the HEIGHT, one above the other.
    Vertical,
}

/// How much room a [`Node::Split`]'s child asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Size {
    /// This many cells, no matter what. The status bar is `Fixed(1)`.
    ///
    /// **Beats the kind's minimum**: if you ask for three cells for
    /// something whose minimum is five, you get three. The minimum decides
    /// when a PROPORTIONAL layout collapses; it does not override an
    /// explicit order.
    Fixed(u16),
    /// Proportional layout of what is left after the fixed ones. The panes.
    Weight(u16),
    /// Whatever its content asks for.
    ///
    /// The tasks strip measures `min(tasks, 6)` rows and is worth ZERO at
    /// rest, and only whoever has the `TaskBoard` in front knows that. It
    /// is replaced by a [`Size::Fixed`] with [`Node::substitute_auto`]
    /// BEFORE laying out, so `resolve` never sees it and stays pure.
    Auto,
}

/// A border a panel docks against.
///
/// Exists for [`Node::dock`]: a sidebar is not "split" from a slot (that is
/// [`Node::split_slot`], which shares ONE's room), it sticks to the side of
/// what is already there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Edge {
    /// Left: first child of a horizontal `Split`.
    Left,
    /// Right: last child of a horizontal `Split`.
    Right,
    /// Top: first child of a vertical `Split`.
    Top,
    /// Bottom: last child of a vertical `Split`.
    Bottom,
}

impl Edge {
    /// The axis this border cuts on.
    #[must_use]
    pub const fn axis(self) -> Dir {
        match self {
            Self::Left | Self::Right => Dir::Horizontal,
            Self::Top | Self::Bottom => Dir::Vertical,
        }
    }

    /// Does it go BEFORE the ones already there?
    #[must_use]
    pub const fn is_front(self) -> bool {
        matches!(self, Self::Left | Self::Top)
    }
}

/// A slot's parameters: an OPAQUE bag only its kind interprets.
///
/// The engine never reads it — it is the client half of the same decision
/// that stops the core from reading it (ADR 0058 D4). For a `browser` it
/// carries the starting directory; for a future `preview`, the line-wrap
/// mode.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Params(serde_json::Map<String, serde_json::Value>);

impl Params {
    /// An empty bag.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A key's value, if it is there.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&serde_json::Value> {
        self.0.get(key)
    }

    /// Sets a key. Each kind uses it with its own; the engine never does.
    pub fn set(&mut self, key: impl Into<String>, value: serde_json::Value) {
        self.0.insert(key.into(), value);
    }

    /// No parameters at all?
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// A named pointer inside the tree, resolved on every frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoleId {
    /// The slot with the focus.
    Active,
    /// Where an operation that needs a second place goes.
    Target,
}

/// Who a slot follows.
///
/// Without this, a side panel is a box with nothing inside: a `metadata`
/// that does not know whose cursor to show shows nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Follow {
    /// Follows whoever holds that role NOW. `Role(Active)` is the useful
    /// default.
    Role(RoleId),
    /// Follows a specific slot, whatever happens to the focus.
    Slot(SlotId),
}

/// A slot's bindings.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bindings {
    /// Who this slot looks at. `None` = nobody (a `browser` looks at
    /// nobody: it is the one being looked at).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follows: Option<Follow>,
}

impl Bindings {
    /// Binds nothing at all? Used by serialization to avoid writing an
    /// empty table for every slot: most slots look at nobody, and an empty
    /// `[...slot.bindings]` is noise in the file a user copies and bytes in
    /// the session body.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.follows.is_none()
    }
}

/// How a [`Node::Tabs`] ends up after an operation: its new children and
/// which one stays active. Receives the current children and the position
/// of the one being operated on.
type ReTab<'a> = dyn Fn(&[Node], usize) -> (Vec<Node>, usize) + 'a;

/// A tree node.
///
/// Three variants and not one more: **tabs are a NODE TYPE, not a
/// feature**, so where they fall decides whether they are workspaces, pane
/// tabs or half a screen alternating views (ADR 0058 D1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Node {
    /// The children split the area along `dir`, according to `sizes`.
    Split {
        /// Where it is cut.
        dir: Dir,
        /// The children, in paint order.
        children: Vec<Node>,
        /// How much each child asks for. Index-parallel to `children`.
        sizes: Vec<Size>,
    },
    /// The children occupy the same area and only one is shown.
    Tabs {
        /// The tabs, in order.
        children: Vec<Node>,
        /// Which one is shown. Out of range is clamped with a diagnostic.
        active: usize,
    },
    /// A leaf: a panel.
    Slot {
        /// Its identity, stable while the layout does not delete it.
        id: SlotId,
        /// Which panel it is.
        kind: KindId,
        /// Whatever that panel needs. The engine does not read it.
        #[serde(default, skip_serializing_if = "Params::is_empty")]
        params: Params,
        /// Who it looks at.
        #[serde(default, skip_serializing_if = "Bindings::is_empty")]
        bindings: Bindings,
    },
}

/// A leaf that is a PANEL: neither a listing nor a chrome row.
fn es_panel(n: &Node) -> bool {
    matches!(n, Node::Slot { kind, .. }
        if !matches!(kind.as_str(), "browser" | "status" | "tasks"))
}

/// A CHROME row: the status bar or the tasks strip. They do not move or
/// receive, and a layout that contains them is not flipped: the status bar
/// sideways would stop being a bar (ADR 0138).
fn es_chrome(n: &Node) -> bool {
    matches!(n, Node::Slot { kind, .. } if matches!(kind.as_str(), "status" | "tasks"))
}

/// A layout's sizes with the border between `pos` and `pos + 1` at fraction
/// `frac` of the `cells` the two occupy (see [`Node::drag_border`]).
///
/// Two WEIGHTED ones keep their sum: what one gains the other loses, and
/// the rest of the layout never knows. Simply renormalizing the pair to a
/// hundred — what it used to do — left a third sibling of weight one
/// against a pair of a hundred: grabbing the border between two listings
/// crushed the one next to it. If the pair's sum is too small to have
/// granularity, the WHOLE layout is multiplied by the same factor, which
/// changes no proportion.
fn drag_pair(sizes: &[Size], pos: usize, frac: f32, pair_cells: u16) -> Vec<Size> {
    /// The pair's minimum weight for the drag to have granularity.
    const WEIGHT_FINO: u32 = 100;
    /// The minimum left to each side, as a fraction.
    const MARGIN: f32 = 0.05;
    let frac = frac.clamp(MARGIN, 1.0 - MARGIN);
    let mut ns = sizes.to_vec();
    let Some(next) = ns.get(pos + 1).copied() else {
        // The last one has no border to its right: what gets dragged is
        // its border with the previous one, and the caller names the
        // border's LEFT slot.
        return ns;
    };
    // In cells, and capped so each side keeps at least one: layout already
    // knows how to collapse what does not fit, but a zero written into the
    // tree stays written.
    let cells = f32::from(pair_cells);
    // Rounding and clipping, in ONE place: `f32` to `u16` truncates and has
    // no sign, so the clamp goes before converting and not after — an `as`
    // on a negative or on 70000 gives no warning.
    let whole = |v: f32| -> u16 {
        let v = v.round().clamp(1.0, f32::from(u16::MAX));
        // Already between 1 and `u16::MAX` with no fractional part: the
        // conversion cannot lose anything.
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "the clamp on the line above leaves the value inside u16 and whole"
        )]
        let v = v as u16;
        v
    };
    let left_cells = (cells * frac).round().clamp(1.0, (cells - 1.0).max(1.0));
    match (ns[pos], next) {
        (Size::Fixed(_), Size::Fixed(_)) => {
            ns[pos] = Size::Fixed(whole(left_cells));
            ns[pos + 1] = Size::Fixed(whole(cells - left_cells));
        }
        // A fixed one against a weighted one: the FIXED one is written and
        // the other keeps whatever is left, which is what layout already
        // did. Writing both would turn a weighted one into a fixed one by
        // dragging its border, and with that it would stop stretching when
        // the window resizes.
        (Size::Fixed(_), _) => ns[pos] = Size::Fixed(whole(left_cells)),
        (_, Size::Fixed(_)) => {
            ns[pos + 1] = Size::Fixed(whole(cells - left_cells));
        }
        (Size::Weight(wa), Size::Weight(wb)) => {
            let sum = u32::from(wa) + u32::from(wb);
            let factor = WEIGHT_FINO.div_ceil(sum.max(1)).max(1);
            if factor > 1 {
                for s in &mut ns {
                    if let Size::Weight(w) = s {
                        *w = u16::try_from(u32::from(*w) * factor).unwrap_or(u16::MAX);
                    }
                }
            }
            let sum = f32::from(u16::try_from(sum * factor).unwrap_or(u16::MAX));
            let left = (sum * frac).round().clamp(1.0, (sum - 1.0).max(1.0));
            ns[pos] = Size::Weight(whole(left));
            ns[pos + 1] = Size::Weight(whole(sum - left));
        }
        _ => {}
    }
    ns
}

/// What searching for what to flip returns (ADR 0138): three cases and not
/// an `Option`, because "not flipped here" has to STOP the search and "not
/// here" has to let it continue. With an `Option` the negative propagated
/// up and the outer layout got flipped.
enum Flip {
    /// Flipped: the new tree.
    Done(Node),
    /// Found, and not flipped.
    Refused,
    /// The slot is not in this subtree, or there is no layout to flip.
    NoThis,
}

/// Where a slot dropped onto another falls (ADR 0138): on one of its four
/// sides, or in the CENTER, which is joining it as a tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DropZone {
    /// To the target's left.
    Left,
    /// To the right.
    Right,
    /// Above.
    Top,
    /// Below.
    Bottom,
    /// As the target's tab.
    Center,
}

impl DropZone {
    /// The zone of a rectangle under point `(x, y)`: the nearest side if it
    /// is less than a QUARTER of it away, and the center otherwise. The
    /// same rule as `zoneOf` in the window (`render/move.ts`).
    #[must_use]
    pub fn at(x: u16, y: u16, rect: Rect) -> Self {
        let frac = |p: u16, o: u16, long: u16| {
            if long == 0 {
                0.5
            } else {
                (f32::from(p.saturating_sub(o)) + 0.5) / f32::from(long)
            }
        };
        let fx = frac(x, rect.x, rect.width);
        let fy = frac(y, rect.y, rect.height);
        let sides = [
            (Self::Left, fx),
            (Self::Right, 1.0 - fx),
            (Self::Top, fy),
            (Self::Bottom, 1.0 - fy),
        ];
        let (side, d) =
            sides.into_iter().fold(
                (Self::Center, f32::INFINITY),
                |m, l| if l.1 < m.1 { l } else { m },
            );
        if d < 0.25 { side } else { Self::Center }
    }

    /// The part of `rect` the zone occupies: one half, or the whole
    /// rectangle for the center. It is what is highlighted while dragging.
    #[must_use]
    pub fn part_of(self, rect: Rect) -> Rect {
        let (w2, h2) = (rect.width / 2, rect.height / 2);
        match self {
            Self::Left => Rect { width: w2, ..rect },
            Self::Right => Rect {
                x: rect.x + w2,
                width: rect.width - w2,
                ..rect
            },
            Self::Top => Rect { height: h2, ..rect },
            Self::Bottom => Rect {
                y: rect.y + h2,
                height: rect.height - h2,
                ..rect
            },
            Self::Center => rect,
        }
    }

    /// The zone's border; `None` for the center.
    #[must_use]
    pub const fn edge(self) -> Option<Edge> {
        match self {
            Self::Left => Some(Edge::Left),
            Self::Right => Some(Edge::Right),
            Self::Top => Some(Edge::Top),
            Self::Bottom => Some(Edge::Bottom),
            Self::Center => None,
        }
    }
}

/// A lone panel, or a tab group made ONLY of panels: what a new panel of
/// the same border can join (phase F). A group with a listing inside is a
/// listing's tab group, and a panel does not go in there.
fn is_a_group_of_panes(n: &Node) -> bool {
    match n {
        Node::Tabs { children, .. } => !children.is_empty() && children.iter().all(es_panel),
        other => es_panel(other),
    }
}

/// The size a new child gets when it enters a layout that already has
/// `hermanos`.
///
/// A [`Size::Weight`] is a PROPORTION, so it only means something next to
/// the other weights: `Weight(1)` means "like one of them" in a layout of
/// ones, but dragging a border leaves the listings at 49/51 and then that
/// same `Weight(1)` is one pixel. It is scaled by the AVERAGE of the
/// sibling weights. A [`Size::Fixed`] or a layout with no weights does not
/// change.
fn weight_between_hermanos(size: Size, hermanos: &[Size]) -> Size {
    let Size::Weight(w) = size else {
        return size;
    };
    let weights: Vec<u32> = hermanos
        .iter()
        .filter_map(|s| match s {
            Size::Weight(p) => Some(u32::from(*p)),
            _ => None,
        })
        .collect();
    if weights.is_empty() {
        return size;
    }
    let n = u32::try_from(weights.len()).unwrap_or(u32::MAX);
    let average = (weights.iter().sum::<u32>() + n / 2) / n;
    Size::Weight(u16::try_from(average.saturating_mul(u32::from(w)).max(1)).unwrap_or(u16::MAX))
}

impl Node {
    /// A slot with no params or bindings.
    #[must_use]
    pub fn slot(id: SlotId, kind: KindId) -> Self {
        Self::Slot {
            id,
            kind,
            params: Params::new(),
            bindings: Bindings::default(),
        }
    }

    /// A slot with bindings: whose view it is.
    ///
    /// Requested by the docked preview, which is the usual `viewer` kind
    /// with a `follows` set — the kind says WHAT is inside and the binding
    /// says WHOSE view it is, which is exactly ADR 0058's separation.
    #[must_use]
    pub fn slot_bound(id: SlotId, kind: KindId, bindings: Bindings) -> Self {
        Self::Slot {
            id,
            kind,
            params: Params::new(),
            bindings,
        }
    }

    /// All the tree's ids in reading order, INCLUDING inactive tabs': a
    /// hidden slot still exists and still has state.
    #[must_use]
    pub fn slot_ids(&self) -> Vec<SlotId> {
        let mut out = Vec::new();
        self.collect_slot_ids(&mut out);
        out
    }

    fn collect_slot_ids(&self, out: &mut Vec<SlotId>) {
        match self {
            Self::Split { children, .. } | Self::Tabs { children, .. } => {
                for c in children {
                    c.collect_slot_ids(out);
                }
            }
            Self::Slot { id, .. } => out.push(*id),
        }
    }

    /// A `Split` of equally weighted children. The common case.
    #[must_use]
    pub fn split(dir: Dir, children: Vec<Node>) -> Self {
        let sizes = vec![Size::Weight(1); children.len()];
        Self::Split {
            dir,
            children,
            sizes,
        }
    }

    /// The subtree's first slot in reading order.
    ///
    /// It is the one asked for its natural size: an `Auto` over a whole
    /// subtree has no choice but to lean on someone, and the first one is
    /// the only one that does not depend on how it is laid out afterward.
    #[must_use]
    pub fn first_slot_id(&self) -> Option<SlotId> {
        match self {
            Self::Split { children, .. } | Self::Tabs { children, .. } => {
                children.iter().find_map(Self::first_slot_id)
            }
            Self::Slot { id, .. } => Some(*id),
        }
    }

    /// The same tree with every [`Size::Auto`] replaced by the
    /// [`Size::Fixed`] `natural` says for that child's first slot.
    ///
    /// The SAVED tree keeps its `Auto`s; the FRAME's tree does not have
    /// any. This way `resolve` needs no closure in its signature — which
    /// all its tests would have to pass — and this function is tested on
    /// its own.
    #[must_use]
    pub fn substitute_auto(&self, natural: &dyn Fn(SlotId) -> (u16, u16)) -> Self {
        match self {
            Self::Split {
                dir,
                children,
                sizes,
            } => {
                let kids: Vec<Self> = children
                    .iter()
                    .map(|c| c.substitute_auto(natural))
                    .collect();
                let new_ones = children
                    .iter()
                    .enumerate()
                    .map(|(i, c)| match sizes.get(i) {
                        Some(Size::Auto) => {
                            let (w, h) = c.first_slot_id().map_or((0, 0), natural);
                            Size::Fixed(match dir {
                                Dir::Horizontal => w,
                                Dir::Vertical => h,
                            })
                        }
                        Some(other) => *other,
                        None => Size::Weight(1),
                    })
                    .collect();
                Self::Split {
                    dir: *dir,
                    children: kids,
                    sizes: new_ones,
                }
            }
            Self::Tabs { children, active } => Self::Tabs {
                children: children
                    .iter()
                    .map(|c| c.substitute_auto(natural))
                    .collect(),
                active: *active,
            },
            Self::Slot { .. } => self.clone(),
        }
    }

    /// Splits slot `id` in two along `dir`, with `new` next to it.
    ///
    /// Both end up with the same weight. If `id` is inside a `Tabs`, the
    /// split goes INSIDE that tab and not around the group: splitting a
    /// tab is splitting what you are looking at, not reorganizing its
    /// siblings.
    ///
    /// # Splitting again on the same axis FLATTENS
    ///
    /// If the slot already lives in a `Split` that runs on `dir`, the new
    /// one enters as its sibling instead of wrapping it in another `Split`.
    /// Nesting, each split took half of the half: three panels ended up at
    /// 1/2, 1/4 and 1/4 instead of thirds, and on the next one the deepest
    /// child fell below its kind's minimum and layout degraded it to tabs —
    /// the just-requested panel vanished from the screen with no word,
    /// with the tree saving it just the same.
    ///
    /// Only if the slot is WEIGHTED. A fixed-size one is docked chrome:
    /// putting another child in its row would steal room from what is next
    /// to it, so that one is split from inside, as always. The new one is
    /// born with the same weight as the one it comes from, which over the
    /// default layout — all at one — is exactly splitting evenly.
    #[must_use]
    pub fn split_slot(&self, id: SlotId, dir: Dir, new: &Self) -> Self {
        match self {
            Self::Slot { id: i, .. } if *i == id => {
                Self::split(dir, vec![self.clone(), new.clone()])
            }
            Self::Slot { .. } => self.clone(),
            Self::Split {
                dir: d,
                children,
                sizes,
            } => {
                if *d == dir
                    && let Some(i) = children
                        .iter()
                        .position(|c| matches!(c, Self::Slot { id: s, .. } if *s == id))
                    && let Size::Weight(weight) = sizes.get(i).copied().unwrap_or(Size::Weight(1))
                {
                    let mut kids = children.clone();
                    let mut tam = sizes.clone();
                    tam.resize(kids.len(), Size::Weight(1));
                    kids.insert(i + 1, new.clone());
                    tam.insert(i + 1, Size::Weight(weight));
                    return Self::Split {
                        dir: *d,
                        children: kids,
                        sizes: tam,
                    };
                }
                Self::Split {
                    dir: *d,
                    sizes: sizes.clone(),
                    children: children
                        .iter()
                        .map(|c| c.split_slot(id, dir, new))
                        .collect(),
                }
            }
            Self::Tabs { children, active } => Self::Tabs {
                active: *active,
                children: children
                    .iter()
                    .map(|c| c.split_slot(id, dir, new))
                    .collect(),
            },
        }
    }

    /// Docks `new` against `edge` of the layout `anchor` lives in.
    ///
    /// The exact spot is the DEEPEST `Split` that contains `anchor` and
    /// runs on `edge`'s axis; it enters there as the first child
    /// (`Left`/`Top`) or the last (`Right`/`Bottom`), with size `size`.
    ///
    /// Looking for that split and not the root is the difference between a
    /// sidebar next to the listings and a sidebar next to EVERYTHING: in
    /// the `orthodox` preset the root is vertical (body, tasks, status
    /// bar), so wrapping the root would leave the status bar and the tasks
    /// strip to the sidebar's right instead of below the listings.
    ///
    /// If no ancestor runs on that axis — a single panel, or a vertical
    /// stack — the whole tree is wrapped in a new `Split`, with what was
    /// there weighted. An `anchor` that is not there returns the tree
    /// intact.
    ///
    /// The opposite is [`Self::close_slot`], which already dissolves the
    /// `Split` left with one child: docking and undocking returns the
    /// starting tree.
    #[must_use]
    pub fn dock(&self, anchor: SlotId, edge: Edge, size: Size, new: &Self) -> Self {
        self.dock_con(anchor, edge, size, new, false)
    }

    /// Like [`Self::dock`], but a PANEL arriving at a border where there is
    /// already a panel — or a group of panels — joins it as a tab, up
    /// front, instead of opening another column or row (spec 2026-09-21,
    /// phase F).
    ///
    /// It is what VS Code does: views on the same border share room and
    /// the activity bar picks which one is shown. With plain `dock`, four
    /// panels on the right used to be four thirty-cell columns and the
    /// listings were left with whatever was left over. The group keeps its
    /// size; closing a tab of a group of two undoes it
    /// ([`Self::close_slot`]).
    ///
    /// A listing never groups, nor acts as a group: two listings side by
    /// side are the orthodox file manager.
    #[must_use]
    pub fn dock_grouped(&self, anchor: SlotId, edge: Edge, size: Size, new: &Self) -> Self {
        self.dock_con(anchor, edge, size, new, true)
    }

    fn dock_con(&self, anchor: SlotId, edge: Edge, size: Size, new: &Self, agrupar: bool) -> Self {
        if !self.contains(anchor) {
            return self.clone();
        }
        self.dock_inner(anchor, edge, size, new, agrupar)
            .unwrap_or_else(|| {
                let (children, sizes) = if edge.is_front() {
                    (vec![new.clone(), self.clone()], vec![size, Size::Weight(1)])
                } else {
                    (vec![self.clone(), new.clone()], vec![Size::Weight(1), size])
                };
                Self::Split {
                    dir: edge.axis(),
                    children,
                    sizes,
                }
            })
    }

    /// `Some` if some `Split` on the path to `anchor` ran on the requested
    /// axis and took `new`; `None` if none did, and then [`Self::dock`]
    /// decides.
    fn dock_inner(
        &self,
        anchor: SlotId,
        edge: Edge,
        size: Size,
        new: &Self,
        agrupar: bool,
    ) -> Option<Self> {
        let kids = match self {
            Self::Split { children, .. } | Self::Tabs { children, .. } => children,
            Self::Slot { .. } => return None,
        };
        let pos = kids.iter().position(|c| c.contains(anchor))?;
        // Inward first: the layout that rules is the DEEPEST one that runs
        // on the axis, not the first one found going down.
        if let Some(inside) = kids[pos].dock_inner(anchor, edge, size, new, agrupar) {
            return Some(self.with_child(pos, inside));
        }
        // A `Tabs` does not accept the dock: putting it inside a tab would
        // make the sidebar disappear when switching tabs, which is exactly
        // what a sidebar does not do. Goes up to the parent.
        let Self::Split {
            dir,
            children,
            sizes,
        } = self
        else {
            return None;
        };
        if *dir != edge.axis() {
            return None;
        }
        let mut nc = children.clone();
        let mut ns = sizes.clone();
        // At the back, but IN FRONT of the trailing chrome rows (the tasks
        // strip and the status bar): a panel docked at the bottom goes
        // above the status bar, as in VS Code, and in the terminal the bar
        // has to stay the last row.
        let at = if edge.is_front() {
            0
        } else {
            nc.len()
                - nc.iter()
                    .rev()
                    .take_while(|c| {
                        matches!(c, Self::Slot { kind, .. }
                            if kind.as_str() == "status" || kind.as_str() == "tasks")
                    })
                    .count()
        };
        // Phase F: if that border already has a PANEL (or a group of
        // panels), the new one joins it as a tab, up front.
        let neighbor = if edge.is_front() {
            Some(0)
        } else {
            at.checked_sub(1)
        };
        if agrupar
            && es_panel(new)
            && let Some(v) = neighbor
            && nc.get(v).is_some_and(is_a_group_of_panes)
        {
            let group = match &nc[v] {
                Self::Tabs { children, .. } => {
                    let mut h = children.clone();
                    h.push(new.clone());
                    h
                }
                other => vec![other.clone(), new.clone()],
            };
            let active = group.len() - 1;
            nc[v] = Self::Tabs {
                children: group,
                active,
            };
            // The group's room is the LARGEST its panels ask for: fixed
            // details at thirty and the viewer behind cannot leave the
            // viewer at thirty columns. A weight beats a fixed one (the
            // one asking for proportional room is the one that needs it
            // most).
            if let Some(actual) = ns.get(v).copied() {
                ns[v] = match (actual, size) {
                    (Size::Fixed(a), Size::Fixed(b)) => Size::Fixed(a.max(b)),
                    (Size::Fixed(_), Size::Weight(_)) => weight_between_hermanos(size, sizes),
                    _ => actual,
                };
            }
            return Some(Self::Split {
                dir: *dir,
                children: nc,
                sizes: ns,
            });
        }
        nc.insert(at, new.clone());
        ns.insert(at.min(ns.len()), weight_between_hermanos(size, sizes));
        Some(Self::Split {
            dir: *dir,
            children: nc,
            sizes: ns,
        })
    }

    /// Closes slot `id`: removes it from its parent.
    ///
    /// A `Split` or a `Tabs` left with ONE child dissolves into it. Returns
    /// `None` if `id` is the root or is not there: closing the last panel
    /// would leave a screen with nothing, and the caller decides that.
    #[must_use]
    pub fn close_slot(&self, id: SlotId) -> Option<Self> {
        let kids = match self {
            Self::Split { children, .. } | Self::Tabs { children, .. } => children,
            Self::Slot { .. } => return None,
        };
        for (i, c) in kids.iter().enumerate() {
            if let Some(changed) = c.close_slot(id) {
                return Some(self.with_child(i, changed));
            }
        }
        let pos = kids.iter().position(|c| c.contains(id))?;
        if kids.len() <= 1 {
            return None;
        }
        match self {
            Self::Split {
                dir,
                children,
                sizes,
            } => {
                let mut nc = children.clone();
                let mut ns = sizes.clone();
                nc.remove(pos);
                if pos < ns.len() {
                    ns.remove(pos);
                }
                if nc.len() == 1 {
                    return nc.into_iter().next();
                }
                Some(Self::Split {
                    dir: *dir,
                    children: nc,
                    sizes: ns,
                })
            }
            Self::Tabs { .. } => self.close_tab(id),
            Self::Slot { .. } => None,
        }
    }

    /// Moves slot `id` next to `target`: on its `zone` side, or as its tab
    /// if the zone is the center (ADR 0138). It is dragging a panel by its
    /// title and dropping it onto another, as in VS Code.
    ///
    /// The target UNIT is the slot, or the tab group it lives in: dropping
    /// to the right of a tab splits the whole group, it does not put it
    /// inside it. If that unit's parent already lays out on the zone's
    /// axis and the unit is weighted, the slot enters as a sibling with the
    /// same weight — three listings end up in thirds, not in 1/2, 1/4,
    /// 1/4; if not, the unit is wrapped in a new, evenly split layout, and
    /// a fixed-width panel keeps its width from outside.
    ///
    /// Does nothing — returns the tree as is — if `id` and `target` are the
    /// same, if either is missing or is chrome (status, tasks), or if `id`
    /// is the only slot. Nothing is created or lost: the moved slot keeps
    /// its id, its kind, its params and its bindings.
    #[must_use]
    pub fn move_slot(&self, id: SlotId, target: SlotId, zone: DropZone) -> Self {
        let movable = |s: SlotId| self.find_slot(s).is_some_and(|n| !es_chrome(n));
        if id == target || !movable(id) || !movable(target) {
            return self.clone();
        }
        let Some(node) = self.find_slot(id).cloned() else {
            return self.clone();
        };
        // The CENTER only joins what is already of the same family: a
        // listing with listings, a panel with panels (ADR 0134). A listing
        // put into the places tabs would live in sixteen columns, and a
        // mixed group would stop being a panel group forever.
        if zone == DropZone::Center
            && self
                .find_slot(target)
                .is_none_or(|t| es_panel(t) != es_panel(&node))
        {
            return self.clone();
        }
        let Some(rest) = self.close_slot(id) else {
            return self.clone();
        };
        match zone.edge() {
            None => rest.add_tab(target, &node),
            Some(edge) => rest
                .place_beside(target, edge, &node)
                .unwrap_or_else(|| self.clone()),
        }
    }

    /// The LEAF node of slot `id`.
    fn find_slot(&self, id: SlotId) -> Option<&Self> {
        match self {
            Self::Slot { id: i, .. } if *i == id => Some(self),
            Self::Slot { .. } => None,
            Self::Split { children, .. } | Self::Tabs { children, .. } => {
                children.iter().find_map(|c| c.find_slot(id))
            }
        }
    }

    /// Is this node the unit that represents `target` in its parent: the
    /// slot itself, or the tab group it is a direct child of?
    fn is_unit_of(&self, target: SlotId) -> bool {
        match self {
            Self::Slot { id, .. } => *id == target,
            Self::Tabs { children, .. } => children
                .iter()
                .any(|c| matches!(c, Self::Slot { id, .. } if *id == target)),
            Self::Split { .. } => false,
        }
    }

    /// `node` beside `edge` of `target`'s unit; `None` if it is not there.
    fn place_beside(&self, target: SlotId, edge: Edge, node: &Self) -> Option<Self> {
        if self.is_unit_of(target) {
            let (children, sizes) = if edge.is_front() {
                (vec![node.clone(), self.clone()], vec![Size::Weight(1); 2])
            } else {
                (vec![self.clone(), node.clone()], vec![Size::Weight(1); 2])
            };
            return Some(Self::Split {
                dir: edge.axis(),
                children,
                sizes,
            });
        }
        let kids = match self {
            Self::Split { children, .. } | Self::Tabs { children, .. } => children,
            Self::Slot { .. } => return None,
        };
        let pos = kids.iter().position(|c| c.contains(target))?;
        // Sibling in the SAME layout, if it runs on the axis and the unit is
        // weighted: this way it splits evenly, like `split_slot`. Never in
        // chrome's layout: that one is not flipped, and what got dropped
        // there could no longer be flipped back with `layout.flip`.
        if let Self::Split {
            dir,
            children,
            sizes,
        } = self
            && *dir == edge.axis()
            && !children.iter().any(es_chrome)
            && children[pos].is_unit_of(target)
        {
            // Next to a FIXED-width panel it also enters as a weighted
            // sibling: splitting it from inside would give half of its
            // sixteen columns to a listing.
            let tam = match sizes.get(pos).copied().unwrap_or(Size::Weight(1)) {
                Size::Weight(weight) => Size::Weight(weight),
                Size::Fixed(_) | Size::Auto => weight_between_hermanos(Size::Weight(1), sizes),
            };
            let mut nc = children.clone();
            let mut ns = sizes.clone();
            ns.resize(nc.len(), Size::Weight(1));
            let at = if edge.is_front() { pos } else { pos + 1 };
            nc.insert(at, node.clone());
            ns.insert(at, tam);
            return Some(Self::Split {
                dir: *dir,
                children: nc,
                sizes: ns,
            });
        }
        let inside = kids[pos].place_beside(target, edge, node)?;
        Some(self.with_child(pos, inside))
    }

    /// Flips the innermost layout that contains `id`: side by side becomes
    /// one above the other, and back (ADR 0138, `layout.flip`).
    ///
    /// What gets flipped is the RUN of weighted siblings around the slot:
    /// in `H[places Fixed(16), a, b]`, `a` and `b` get stacked and places
    /// stays a sixteen-wide column — flipping the whole row would have
    /// turned it into a band and the width would not come back on flipping
    /// again. If the run is the whole layout, the layout is flipped; if it
    /// is only part of it, that part becomes its own layout, with the
    /// weight it summed to.
    ///
    /// A layout with chrome inside (status bar, tasks) is not flipped —
    /// the bar sideways would stop being a bar — nor is a run of one. Then
    /// NOTHING happens: the refusal does not propagate up to try the outer
    /// layout.
    #[must_use]
    pub fn flip(&self, id: SlotId) -> Self {
        match self.flip_inner(id) {
            Flip::Done(n) => n,
            Flip::Refused | Flip::NoThis => self.clone(),
        }
    }

    fn flip_inner(&self, id: SlotId) -> Flip {
        let kids = match self {
            Self::Split { children, .. } | Self::Tabs { children, .. } => children,
            Self::Slot { .. } => return Flip::NoThis,
        };
        let Some(pos) = kids.iter().position(|c| c.contains(id)) else {
            return Flip::NoThis;
        };
        match kids[pos].flip_inner(id) {
            Flip::Done(inside) => return Flip::Done(self.with_child(pos, inside)),
            Flip::Refused => return Flip::Refused,
            Flip::NoThis => {}
        }
        let Self::Split {
            dir,
            children,
            sizes,
        } = self
        else {
            return Flip::NoThis;
        };
        if children.iter().any(es_chrome) {
            return Flip::Refused;
        }
        let other = match dir {
            Dir::Horizontal => Dir::Vertical,
            Dir::Vertical => Dir::Horizontal,
        };
        let weighs = |i: usize| matches!(sizes.get(i), Some(Size::Weight(_)) | None);
        if !weighs(pos) {
            return Flip::Refused;
        }
        let mut from = pos;
        while from > 0 && weighs(from - 1) {
            from -= 1;
        }
        let mut until = pos + 1;
        while until < children.len() && weighs(until) {
            until += 1;
        }
        if until - from < 2 {
            return Flip::Refused;
        }
        if from == 0 && until == children.len() {
            return Flip::Done(Self::Split {
                dir: other,
                children: children.clone(),
                sizes: sizes.clone(),
            });
        }
        let how_much = |i: usize| match sizes.get(i) {
            Some(Size::Weight(w)) => u32::from(*w),
            _ => 1,
        };
        let total = u16::try_from((from..until).map(how_much).sum::<u32>()).unwrap_or(u16::MAX);
        let streak = Self::Split {
            dir: other,
            children: children[from..until].to_vec(),
            sizes: (from..until)
                .map(|i| sizes.get(i).copied().unwrap_or(Size::Weight(1)))
                .collect(),
        };
        let mut nc = children[..from].to_vec();
        nc.push(streak);
        nc.extend_from_slice(&children[until..]);
        let mut ns: Vec<Size> = (0..from)
            .map(|i| sizes.get(i).copied().unwrap_or(Size::Weight(1)))
            .collect();
        ns.push(Size::Weight(total.max(1)));
        ns.extend(
            (until..children.len()).map(|i| sizes.get(i).copied().unwrap_or(Size::Weight(1))),
        );
        Flip::Done(Self::Split {
            dir: *dir,
            children: nc,
            sizes: ns,
        })
    }

    /// Changes the size of the child that contains `id` by `delta`.
    ///
    /// A WEIGHTED child moves in weight, between 1 and 10. A FIXED child
    /// moves in CELLS, two per keystroke, between 2 and 100: a sidebar
    /// asked for a specific width, and until #227 that meant the keyboard
    /// could not change it — which is an imposed width, not a chosen one.
    /// The lower cap is not cosmetic: at zero the panel disappears and with
    /// it the way to bring it back.
    ///
    /// [`Size::Auto`] is not touched: it is replaced by a fixed one BEFORE
    /// laying out, so a number saved here would be overwritten by the next
    /// frame and the key would look broken.
    #[must_use]
    pub fn resize(&self, id: SlotId, delta: i16) -> Self {
        /// Cells per keystroke on a fixed child.
        const PASO: i32 = 2;
        self.map_split_of(id, &|sizes, pos| {
            let mut ns = sizes.to_vec();
            match ns.get(pos) {
                Some(Size::Weight(w)) => {
                    let new = i32::from(*w).saturating_add(i32::from(delta)).clamp(1, 10);
                    ns[pos] = Size::Weight(u16::try_from(new).unwrap_or(1));
                }
                Some(Size::Fixed(n)) => {
                    let new = i32::from(*n)
                        .saturating_add(i32::from(delta).saturating_mul(PASO))
                        .clamp(2, 100);
                    ns[pos] = Size::Fixed(u16::try_from(new).unwrap_or(2));
                }
                Some(Size::Auto) | None => {}
            }
            ns
        })
    }

    /// Sets the border BETWEEN `id` and its right (or bottom) sibling at
    /// fraction `frac` of the space the two occupy together.
    ///
    /// It is the DRAG primitive, and that is why it is absolute and not a
    /// step: [`Self::resize`] moves two cells per keystroke, which is what
    /// a key wants; a mouse says WHERE the border goes, and turning that
    /// into a string of steps would give a border that never reaches where
    /// the pointer is.
    ///
    /// The sum of the two sizes is KEPT: what one gains the other loses,
    /// and the rest of the row never knows. A five-slot `Split` where
    /// dragging a border repositioned all five would be a gesture that
    /// touches what nobody grabbed.
    ///
    /// With weights the pair is renormalized to a hundred (`WEIGHT_FINO`) so
    /// the drag has granularity: two slots by default are `Weight(1)` and
    /// `Weight(1)`, and over that pair only the exact half would exist.
    ///
    /// `frac` is clamped so neither of the two disappears: a slot at zero
    /// takes with it the way to bring it back.
    ///
    /// [`Size::Auto`] is not touched, for the same reason as in
    /// [`Self::resize`]: layout replaces it and a number saved here would
    /// be overwritten by the next frame.
    /// `pair_cells` is what the two occupy together, in layout cells.
    /// Whoever paints knows it, not the tree: a [`Size::Fixed`] is measured
    /// in cells and a fraction alone is not enough to write it.
    #[must_use]
    pub fn drag_border(&self, id: SlotId, frac: f32, pair_cells: u16) -> Self {
        self.map_split_of(id, &|sizes, pos| drag_pair(sizes, pos, frac, pair_cells))
    }

    /// The two sides of the border between `a` and `b`: the slots of the
    /// child that contains `a` and those of the NEXT child, which contains
    /// `b`, in the layout where the two are consecutive siblings.
    ///
    /// It is what whoever drags has to MEASURE: the border between a
    /// listing and the details panel does not separate that listing from
    /// the details, it separates the WHOLE body (both listings) from the
    /// details, and measuring only the listing gave another pair's
    /// fraction — the border jumped or did not follow the pointer. `None`
    /// if they are not neighbors in any layout.
    #[must_use]
    pub fn border_pair(&self, a: SlotId, b: SlotId) -> Option<(Vec<SlotId>, Vec<SlotId>)> {
        let kids = match self {
            Self::Split { children, .. } | Self::Tabs { children, .. } => children,
            Self::Slot { .. } => return None,
        };
        let pos = kids.iter().position(|c| c.contains(a))?;
        if kids[pos].contains(b) {
            return kids[pos].border_pair(a, b);
        }
        if matches!(self, Self::Split { .. })
            && let Some(next) = kids.get(pos + 1)
            && next.contains(b)
        {
            return Some((kids[pos].slot_ids(), next.slot_ids()));
        }
        None
    }

    /// Like [`Self::drag_border`], but on the border between `a` and `b` in
    /// the layout where they are neighbors ([`Self::border_pair`]), with
    /// `frac` and `pair_cells` measured over the whole TWO children.
    /// This way the border that was grabbed moves, even if `a` is the last
    /// one of its own layout.
    #[must_use]
    pub fn drag_border_between(&self, a: SlotId, b: SlotId, frac: f32, pair_cells: u16) -> Self {
        let kids = match self {
            Self::Split { children, .. } | Self::Tabs { children, .. } => children,
            Self::Slot { .. } => return self.clone(),
        };
        let Some(pos) = kids.iter().position(|c| c.contains(a)) else {
            return self.clone();
        };
        if kids[pos].contains(b) {
            let inside = kids[pos].drag_border_between(a, b, frac, pair_cells);
            return self.with_child(pos, inside);
        }
        if let Self::Split {
            dir,
            children,
            sizes,
        } = self
            && children.get(pos + 1).is_some_and(|c| c.contains(b))
        {
            return Self::Split {
                dir: *dir,
                children: children.clone(),
                sizes: drag_pair(sizes, pos, frac, pair_cells),
            };
        }
        self.clone()
    }

    /// Leaves all of slot `id`'s weighted siblings with the same weight.
    #[must_use]
    pub fn equalize(&self, id: SlotId) -> Self {
        self.map_split_of(id, &|sizes, _| {
            sizes
                .iter()
                .map(|s| match s {
                    Size::Weight(_) => Size::Weight(1),
                    other => *other,
                })
                .collect()
        })
    }

    /// The sizes the `Split` that contains `id` lays out with, and its
    /// child's position.
    ///
    /// It is what [`Self::resize`] changes, to be able to LOOK AT IT:
    /// without this, a panel-width test ends up comparing whole trees or
    /// calling `resize` with an id the caller does not really produce —
    /// which is exactly how #244 M1 went unnoticed.
    ///
    /// ```
    /// use norte_frontend::layout::{Dir, KindId, Node, Size, SlotId};
    ///
    /// let arbol = Node::split(
    ///     Dir::Horizontal,
    ///     vec![
    ///         Node::slot(SlotId(1), KindId::browser()),
    ///         Node::slot(SlotId(2), KindId::browser()),
    ///     ],
    /// );
    /// let (sizes, pos) = arbol.sizes_of(SlotId(2)).expect("is in a split");
    /// assert_eq!((sizes[pos], pos), (Size::Weight(1), 1));
    /// assert!(Node::slot(SlotId(1), KindId::browser()).sizes_of(SlotId(1)).is_none());
    /// ```
    #[must_use]
    pub fn sizes_of(&self, id: SlotId) -> Option<(&[Size], usize)> {
        let kids = match self {
            Self::Split { children, .. } | Self::Tabs { children, .. } => children,
            Self::Slot { .. } => return None,
        };
        for c in kids {
            if c.contains(id)
                && !matches!(c, Self::Slot { .. })
                && let Some(inside) = c.sizes_of(id)
            {
                return Some(inside);
            }
        }
        if let Self::Split {
            children, sizes, ..
        } = self
            && let Some(pos) = children.iter().position(|c| c.contains(id))
        {
            return Some((sizes.as_slice(), pos));
        }
        None
    }

    /// Applies `f` to the sizes of the `Split` that contains `id`, giving
    /// it the position of the child that contains it.
    fn map_split_of(&self, id: SlotId, f: &dyn Fn(&[Size], usize) -> Vec<Size>) -> Self {
        let kids = match self {
            Self::Split { children, .. } | Self::Tabs { children, .. } => children,
            Self::Slot { .. } => return self.clone(),
        };
        for (i, c) in kids.iter().enumerate() {
            if c.contains(id) && !matches!(c, Self::Slot { .. }) {
                let inside = c.map_split_of(id, f);
                if inside != *c {
                    return self.with_child(i, inside);
                }
            }
        }
        if let Self::Split {
            dir,
            children,
            sizes,
        } = self
            && let Some(pos) = children.iter().position(|c| c.contains(id))
        {
            return Self::Split {
                dir: *dir,
                children: children.clone(),
                sizes: f(sizes, pos),
            };
        }
        self.clone()
    }

    /// The slots that WOULD BE SEEN: like [`Self::slot_ids`], but only the
    /// active tab of each [`Node::Tabs`].
    ///
    /// Not the same as a layout's placements — this does not know whether
    /// anything fits — and that is why it exists: it is needed to know who
    /// stays visible right AFTER touching the tree, before there is a
    /// frame to lay out.
    #[must_use]
    pub fn visible_slot_ids(&self) -> Vec<SlotId> {
        let mut out = Vec::new();
        self.collect_visible(&mut out);
        out
    }

    fn collect_visible(&self, out: &mut Vec<SlotId>) {
        match self {
            Self::Split { children, .. } => {
                for c in children {
                    c.collect_visible(out);
                }
            }
            Self::Tabs { children, active } => {
                if let Some(c) = children.get(*active).or_else(|| children.first()) {
                    c.collect_visible(out);
                }
            }
            Self::Slot { id, .. } => out.push(*id),
        }
    }

    /// Slot `id`'s kind, if the tree contains it.
    #[must_use]
    pub fn kind_of(&self, id: SlotId) -> Option<&KindId> {
        match self {
            Self::Split { children, .. } | Self::Tabs { children, .. } => {
                children.iter().find_map(|c| c.kind_of(id))
            }
            Self::Slot { id: this, kind, .. } if *this == id => Some(kind),
            Self::Slot { .. } => None,
        }
    }

    /// Slot `id`'s bindings, if the tree contains it.
    #[must_use]
    pub fn bindings_of(&self, id: SlotId) -> Option<&Bindings> {
        match self {
            Self::Split { children, .. } | Self::Tabs { children, .. } => {
                children.iter().find_map(|c| c.bindings_of(id))
            }
            Self::Slot {
                id: this, bindings, ..
            } if *this == id => Some(bindings),
            Self::Slot { .. } => None,
        }
    }

    /// Is slot `id` in this subtree?
    #[must_use]
    pub fn contains(&self, id: SlotId) -> bool {
        self.slot_ids().contains(&id)
    }

    /// The same tree with slot `id` put into a single-tab [`Node::Tabs`].
    /// If it is already a direct child of a `Tabs`, nothing changes.
    #[must_use]
    pub fn wrap_in_tabs(&self, id: SlotId) -> Self {
        match self {
            Self::Tabs { children, active } => {
                if children
                    .iter()
                    .any(|c| matches!(c, Self::Slot { id: i, .. } if *i == id))
                {
                    return self.clone();
                }
                Self::Tabs {
                    children: children.iter().map(|c| c.wrap_in_tabs(id)).collect(),
                    active: *active,
                }
            }
            Self::Split {
                dir,
                children,
                sizes,
            } => Self::Split {
                dir: *dir,
                sizes: sizes.clone(),
                children: children.iter().map(|c| c.wrap_in_tabs(id)).collect(),
            },
            Self::Slot { id: i, .. } if *i == id => Self::Tabs {
                children: vec![self.clone()],
                active: 0,
            },
            Self::Slot { .. } => self.clone(),
        }
    }

    /// Opens `new` as a tab next to `id`, and leaves it active.
    ///
    /// If `id` was not in tabs, it wraps it first: opening a tab from a
    /// lone panel is what turns that panel into the first of a group, and
    /// asking the user for two steps for that would make no sense.
    #[must_use]
    pub fn add_tab(&self, id: SlotId, new: &Self) -> Self {
        let wrapped = self.wrap_in_tabs(id);
        wrapped.insert_tab_near(id, new).unwrap_or(wrapped)
    }

    fn insert_tab_near(&self, id: SlotId, new: &Self) -> Option<Self> {
        let kids = match self {
            Self::Split { children, .. } | Self::Tabs { children, .. } => children,
            Self::Slot { .. } => return None,
        };
        // Deeper inside first: the `Tabs` that rules is the INNER one, not
        // the one wrapping half the screen.
        for (i, c) in kids.iter().enumerate() {
            if let Some(changed) = c.insert_tab_near(id, new) {
                return Some(self.with_child(i, changed));
            }
        }
        if let Self::Tabs { children, .. } = self
            && let Some(pos) = children.iter().position(|c| c.contains(id))
        {
            let mut new_ones = children.clone();
            new_ones.insert(pos + 1, new.clone());
            return Some(Self::Tabs {
                children: new_ones,
                active: pos + 1,
            });
        }
        None
    }

    /// Closes the tab that contains `id`.
    ///
    /// A `Tabs` left with ONE child DISSOLVES into it: a one-tab group is
    /// not a group, and leaving it would paint a tab bar with a single
    /// entry forever.
    ///
    /// Returns `None` if `id` is not inside any `Tabs` — closing a lone
    /// panel is `layout.close-slot`, not `pane.tab-close`.
    #[must_use]
    pub fn close_tab(&self, id: SlotId) -> Option<Self> {
        let kids = match self {
            Self::Split { children, .. } | Self::Tabs { children, .. } => children,
            Self::Slot { .. } => return None,
        };
        for (i, c) in kids.iter().enumerate() {
            if let Some(changed) = c.close_tab(id) {
                return Some(self.with_child(i, changed));
            }
        }
        if let Self::Tabs { children, active } = self
            && children.len() > 1
            && let Some(pos) = children.iter().position(|c| c.contains(id))
        {
            let mut new_ones = children.clone();
            new_ones.remove(pos);
            if new_ones.len() == 1 {
                return new_ones.into_iter().next();
            }
            // Closing a tab BEFORE the active one shifts the index; if not,
            // active would end up naming the one next to it. The clamp
            // goes AFTER the shift: the other way round both apply and
            // active falls one position too far.
            let act = if pos < *active {
                active.saturating_sub(1)
            } else {
                *active
            }
            .min(new_ones.len() - 1);
            return Some(Self::Tabs {
                children: new_ones,
                active: act,
            });
        }
        None
    }

    /// The tabs of the group that contains `id`: the slot that heads each
    /// one and which is active. `None` if `id` is not in a group.
    ///
    /// Used by the tab bar's render, and that is why it returns the FIRST
    /// slot of each tab: that is who the title is taken from.
    #[must_use]
    pub fn tabs_of(&self, id: SlotId) -> Option<(Vec<SlotId>, usize)> {
        let kids = match self {
            Self::Split { children, .. } | Self::Tabs { children, .. } => children,
            Self::Slot { .. } => return None,
        };
        for c in kids {
            if let Some(v) = c.tabs_of(id) {
                return Some(v);
            }
        }
        if let Self::Tabs { children, active } = self
            && children.iter().any(|c| c.contains(id))
        {
            return Some((
                children.iter().filter_map(Self::first_slot_id).collect(),
                (*active).min(children.len().saturating_sub(1)),
            ));
        }
        None
    }

    /// The same tree with slot `id` made VISIBLE: activates its tab in
    /// every group along the path (#329).
    ///
    /// Exists because [`Self::slot_ids`] and [`Self::visible_slot_ids`]
    /// answer two different questions — "does it exist?" and "is it
    /// shown?" — and there was a third one with no answer: "make it
    /// shown". Without it, whoever found a hidden slot could only send it
    /// the keyboard, which is focusing something the reader does not have
    /// in front of them.
    ///
    /// Walks the WHOLE path and not just the inner group: activating the
    /// inner tab while leaving the outer one on another leaves the slot
    /// just as invisible, and the caller would believe it had been shown.
    /// A slot that is not there returns the tree unchanged — this enforces
    /// an invariant, it does not run a gesture.
    ///
    /// One that is already shown is returned unchanged except in one case,
    /// worth stating: the type allows an out-of-range `active`, which
    /// [`Self::visible_slot_ids`] and layout both clamp to the first one.
    /// On one like that, revealing the slot that WAS ALREADY shown writes
    /// the real index. It normalizes, it does not move anything.
    ///
    /// ```
    /// use norte_frontend::layout::{KindId, Node, SlotId};
    /// let arbol = Node::Tabs {
    ///     children: vec![
    ///         Node::slot(SlotId(1), KindId::browser()),
    ///         Node::slot(SlotId(2), KindId::new("log")),
    ///     ],
    ///     active: 0,
    /// };
    /// assert!(!arbol.visible_slot_ids().contains(&SlotId(2)));
    /// assert!(arbol.reveal(SlotId(2)).visible_slot_ids().contains(&SlotId(2)));
    /// ```
    #[must_use]
    pub fn reveal(&self, id: SlotId) -> Self {
        match self {
            Self::Slot { .. } => self.clone(),
            Self::Split {
                dir,
                children,
                sizes,
            } => Self::Split {
                dir: *dir,
                children: children
                    .iter()
                    .map(|c| {
                        if c.contains(id) {
                            c.reveal(id)
                        } else {
                            c.clone()
                        }
                    })
                    .collect(),
                sizes: sizes.clone(),
            },
            // The incoming `active` is deliberately not read: revealing
            // does not keep it or move it one step, it SETS it to the tab
            // that contains the slot. That is the whole gesture.
            Self::Tabs { children, .. } => {
                let Some(pos) = children.iter().position(|c| c.contains(id)) else {
                    return self.clone();
                };
                Self::Tabs {
                    children: children
                        .iter()
                        .enumerate()
                        .map(|(i, c)| if i == pos { c.reveal(id) } else { c.clone() })
                        .collect(),
                    active: pos,
                }
            }
        }
    }

    /// Leaves tab `i` of the group that contains `id` active.
    #[must_use]
    pub fn set_active_for(&self, id: SlotId, i: usize) -> Self {
        self.map_tabs_of(id, &|children, _| {
            (children.to_vec(), i.min(children.len() - 1))
        })
    }

    /// Moves the tab that contains `id` `delta` positions, without going
    /// out of range.
    #[must_use]
    pub fn move_tab(&self, id: SlotId, delta: isize) -> Self {
        self.map_tabs_of(id, &|children, pos| {
            let dest = pos
                .saturating_add_signed(delta)
                .min(children.len().saturating_sub(1));
            let mut new_ones = children.to_vec();
            let who = new_ones.remove(pos);
            new_ones.insert(dest, who);
            (new_ones, dest)
        })
    }

    /// Applies `f` to the `Tabs` that contains `id`, giving it its children
    /// and the position of the one that contains it, and expecting the new
    /// children and the active one.
    fn map_tabs_of(&self, id: SlotId, f: &ReTab<'_>) -> Self {
        let kids = match self {
            Self::Split { children, .. } | Self::Tabs { children, .. } => children,
            Self::Slot { .. } => return self.clone(),
        };
        for (i, c) in kids.iter().enumerate() {
            if c.contains(id) && !matches!(c, Self::Slot { .. }) {
                let inside = c.map_tabs_of(id, f);
                if inside != *c {
                    return self.with_child(i, inside);
                }
            }
        }
        if let Self::Tabs { children, .. } = self
            && let Some(pos) = children.iter().position(|c| c.contains(id))
        {
            let (new_ones, act) = f(children, pos);
            return Self::Tabs {
                children: new_ones,
                active: act,
            };
        }
        self.clone()
    }

    /// The same node with child `i` replaced.
    fn with_child(&self, i: usize, child: Self) -> Self {
        match self {
            Self::Split {
                dir,
                children,
                sizes,
            } => {
                let mut new_ones = children.clone();
                if let Some(slot) = new_ones.get_mut(i) {
                    *slot = child;
                }
                Self::Split {
                    dir: *dir,
                    children: new_ones,
                    sizes: sizes.clone(),
                }
            }
            Self::Tabs { children, active } => {
                let mut new_ones = children.clone();
                if let Some(slot) = new_ones.get_mut(i) {
                    *slot = child;
                }
                Self::Tabs {
                    children: new_ones,
                    active: *active,
                }
            }
            Self::Slot { .. } => self.clone(),
        }
    }

    /// A copy whose slots are numbered from `base`, plus the old -> new
    /// map.
    ///
    /// It is what keeps two profiles from stepping on each other (spec
    /// 2026-08-26, D5): the factory layouts ALL use 1..=8, so adopting the
    /// same one in two profiles without reassigning leaves the two sharing
    /// slot 1 — same directory, same history, same marks.
    ///
    /// The map is NOT a convenience. `[profile.start]` comes indexed by the
    /// ids the profile's layout file writes, so applying it after
    /// rebasing requires the translation; returning only the tree would
    /// leave those keys useless.
    ///
    /// The assignment order is [`Self::slot_ids`]'s, which is reading
    /// order: deterministic, and so the same tree rebased twice from the
    /// same base gives the same result.
    ///
    /// # Where `base` comes from
    ///
    /// From [`crate::session::SessionBody::next_slot_base`], and nowhere
    /// else. The "does not collide" property lives ENTIRELY there: looking
    /// only at the active profile's tree would give a base that lands on
    /// top of the orphaned slots, which are exactly the ones nobody is
    /// looking at when it happens. And the rebased tree goes into
    /// `layouts` BEFORE asking for a base again, or two profiles would
    /// rebase from the same number.
    ///
    /// With no free room above, returns the tree UNTOUCHED and an empty
    /// map: the caller stays as it was instead of receiving a tree with
    /// repeated ids.
    ///
    /// ```
    /// use norte_frontend::layout::{Dir, KindId, Node, SlotId};
    ///
    /// let arbol = Node::split(
    ///     Dir::Horizontal,
    ///     vec![
    ///         Node::slot(SlotId(1), KindId::browser()),
    ///         Node::slot(SlotId(2), KindId::browser()),
    ///     ],
    /// );
    /// let (nuevo, map) = arbol.rebase_slot_ids(100);
    /// assert_eq!(nuevo.slot_ids(), vec![SlotId(100), SlotId(101)]);
    /// assert_eq!(map[&SlotId(2)], SlotId(101));
    /// ```
    #[must_use]
    pub fn rebase_slot_ids(&self, base: u32) -> (Self, std::collections::BTreeMap<SlotId, SlotId>) {
        let mut map = std::collections::BTreeMap::new();
        let mut next = base;
        for id in self.slot_ids() {
            // A tree with repeated ids is inconsistent from the start
            // (`duplicate_slot_ids` says so and `validate` rejects it); if
            // one arrives, the two slots keep sharing the id instead of one
            // taking a number nobody gave it.
            if map.contains_key(&id) {
                continue;
            }
            // With no room above the TREE IS RETURNED AS IS, and this is
            // not excessive caution: saturating was worse than bailing
            // out. `saturating_add` would leave every following slot at
            // `u32::MAX`, so a healthy input tree came out with REPEATED
            // ids; that tree gets saved in `layouts`, and the next
            // `from_value` validates it and returns `BadLayout` for the
            // WHOLE body — every profile's state, not just the broken
            // one's. That is the loss ADR 0059 promises does not happen.
            let Some(cap) = next.checked_add(1) else {
                return (self.clone(), std::collections::BTreeMap::new());
            };
            map.insert(id, SlotId(next));
            next = cap;
        }
        (self.remap_slot_ids(&map), map)
    }

    /// Applies an id map to a copy of the tree. What is not in the map
    /// stays as it is.
    fn remap_slot_ids(&self, map: &std::collections::BTreeMap<SlotId, SlotId>) -> Self {
        match self {
            Self::Split {
                dir,
                children,
                sizes,
            } => Self::Split {
                dir: *dir,
                children: children.iter().map(|c| c.remap_slot_ids(map)).collect(),
                sizes: sizes.clone(),
            },
            Self::Tabs { children, active } => Self::Tabs {
                children: children.iter().map(|c| c.remap_slot_ids(map)).collect(),
                active: *active,
            },
            Self::Slot {
                id,
                kind,
                params,
                bindings,
            } => Self::Slot {
                id: map.get(id).copied().unwrap_or(*id),
                kind: kind.clone(),
                params: params.clone(),
                bindings: *bindings,
            },
        }
    }

    /// The repeated ids, if there are any. A layout with two slots of the
    /// same id is inconsistent and it is NOT guessed which one wins.
    #[must_use]
    pub fn duplicate_slot_ids(&self) -> Vec<SlotId> {
        let mut count: BTreeMap<SlotId, usize> = BTreeMap::new();
        for id in self.slot_ids() {
            *count.entry(id).or_default() += 1;
        }
        count
            .into_iter()
            .filter_map(|(id, n)| (n > 1).then_some(id))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **Bringing a hidden slot to light activates ITS tab** (#329).
    ///
    /// The panel bar and the toggles ask "does it exist?" and act as if
    /// they had asked "is it visible?". This is the missing half: being
    /// able to answer "make it visible".
    #[test]
    fn revealing_activates_the_slots_tab() {
        let tree = Node::Tabs {
            children: vec![
                Node::slot(SlotId(1), KindId::browser()),
                Node::slot(SlotId(2), KindId::new("log")),
            ],
            active: 0,
        };
        assert!(!tree.visible_slot_ids().contains(&SlotId(2)));
        let revealed = tree.reveal(SlotId(2));
        assert!(revealed.visible_slot_ids().contains(&SlotId(2)));
        assert!(
            !revealed.visible_slot_ids().contains(&SlotId(1)),
            "activating a tab hides its sibling: that is what it means"
        );
    }

    /// And it does so through EVERY group along the path, not just the
    /// innermost one.
    ///
    /// With nested groups, activating the inner one and leaving the outer
    /// one on another tab leaves the slot as invisible as it was, and the
    /// caller would believe it had shown it.
    #[test]
    fn reveal_crosses_the_nested_groups() {
        let inner = Node::Tabs {
            children: vec![
                Node::slot(SlotId(3), KindId::browser()),
                Node::slot(SlotId(4), KindId::new("log")),
            ],
            active: 0,
        };
        let tree = Node::Tabs {
            children: vec![Node::slot(SlotId(5), KindId::browser()), inner],
            active: 0,
        };
        assert!(!tree.visible_slot_ids().contains(&SlotId(4)));
        let revealed = tree.reveal(SlotId(4));
        assert!(
            revealed.visible_slot_ids().contains(&SlotId(4)),
            "the outer group was still showing the other tab"
        );
    }

    /// With a `Split` along the path, revealing respects EVERYTHING else:
    /// the sizes, and the active tab of a group that does not contain the
    /// slot.
    ///
    /// The risk of a function that rebuilds the tree is losing something
    /// along the way that nobody watches in the test, and here what would
    /// get lost is measurements: a `Split` that comes back with default
    /// weights spreads the screen out differently without anything turning
    /// red.
    #[test]
    fn revealing_preserves_sizes_and_unrelated_groups() {
        let other = Node::Tabs {
            children: vec![
                Node::slot(SlotId(10), KindId::browser()),
                Node::slot(SlotId(11), KindId::browser()),
            ],
            active: 1,
        };
        let tree = Node::Split {
            dir: Dir::Horizontal,
            sizes: vec![Size::Fixed(24), Size::Weight(1)],
            children: vec![
                other,
                Node::Tabs {
                    children: vec![
                        Node::slot(SlotId(1), KindId::browser()),
                        Node::slot(SlotId(2), KindId::new("log")),
                    ],
                    active: 0,
                },
            ],
        };
        let revealed = tree.reveal(SlotId(2));
        assert!(revealed.visible_slot_ids().contains(&SlotId(2)));
        assert!(
            revealed.visible_slot_ids().contains(&SlotId(11)),
            "the group next door is untouched: it does not contain the slot"
        );
        let (sizes, _) = revealed.sizes_of(SlotId(2)).expect("still in the split");
        assert_eq!(
            (sizes[0], sizes[1]),
            (Size::Fixed(24), Size::Weight(1)),
            "rebuilding the split carried off the measurements"
        );
    }

    /// A slot that is already visible — or that is not there — moves
    /// nothing: revealing is not a gesture, it is an invariant being
    /// ensured.
    #[test]
    fn revealing_what_is_already_visible_does_not_change_the_tree() {
        let tree = Node::Tabs {
            children: vec![
                Node::slot(SlotId(1), KindId::browser()),
                Node::slot(SlotId(2), KindId::new("log")),
            ],
            active: 1,
        };
        assert_eq!(tree.reveal(SlotId(2)), tree);
        assert_eq!(
            tree.reveal(SlotId(99)),
            tree,
            "and one that is not there either"
        );
    }

    /// Dragging the border puts the slot where the pointer says, and what
    /// one gains its neighbor loses.
    ///
    /// With weights the pair renormalizes: two default slots are
    /// `Weight(1)` and `Weight(1)`, and on that pair the only possible
    /// border would be the exact middle — a drag that can only land in the
    /// center is not a drag.
    #[test]
    fn dragging_the_edge_splits_the_pair() {
        let tree = Node::split(
            Dir::Horizontal,
            vec![
                Node::slot(SlotId(1), KindId::browser()),
                Node::slot(SlotId(2), KindId::browser()),
            ],
        );
        let moved = tree.drag_border(SlotId(1), 0.25, 80);
        let (sizes, pos) = moved.sizes_of(SlotId(1)).expect("is in a split");
        assert_eq!(pos, 0);
        assert_eq!(
            (sizes[0], sizes[1]),
            (Size::Weight(25), Size::Weight(75)),
            "a quarter for the one on the left, and the rest for the other"
        );
    }

    /// Neither one nor the other can disappear: a slot at zero takes with
    /// it the way to give it back.
    #[test]
    fn dragging_to_the_end_leaves_room_for_both() {
        let tree = Node::split(
            Dir::Horizontal,
            vec![
                Node::slot(SlotId(1), KindId::browser()),
                Node::slot(SlotId(2), KindId::browser()),
            ],
        );
        for frac in [-3.0, 0.0, 1.0, 4.0] {
            let moved = tree.drag_border(SlotId(1), frac, 80);
            let (sizes, _) = moved.sizes_of(SlotId(1)).expect("split");
            for s in &sizes[..2] {
                assert!(
                    matches!(s, Size::Weight(w) if *w >= 1),
                    "with frac={frac} someone was left with no room: {sizes:?}"
                );
            }
        }
    }

    /// A FIXED one is written in cells — that is what it means — and its
    /// weighted neighbor does not turn fixed: if it did, it would stop
    /// stretching when the window is resized.
    #[test]
    fn dragging_the_edge_of_a_fixed_one_writes_it_in_cells() {
        let tree = Node::Split {
            dir: Dir::Horizontal,
            children: vec![
                Node::slot(SlotId(1), KindId::new("places")),
                Node::slot(SlotId(2), KindId::browser()),
            ],
            sizes: vec![Size::Fixed(16), Size::Weight(1)],
        };
        let moved = tree.drag_border(SlotId(1), 0.5, 100);
        let (sizes, _) = moved.sizes_of(SlotId(1)).expect("split");
        assert_eq!(sizes[0], Size::Fixed(50), "half of a hundred cells");
        assert_eq!(sizes[1], Size::Weight(1), "the weighted one stays weighted");
    }

    /// The factory layouts use 1..=8, ALL of them. Without rebasing, two
    /// profiles share slot 1 and step on each other's directory and
    /// history — which is exactly the bug profiles exist to fix.
    #[test]
    fn rebase_reassigns_from_the_base_and_returns_the_map() {
        let tree = Node::split(
            Dir::Horizontal,
            vec![
                Node::slot(SlotId(1), KindId::browser()),
                Node::slot(SlotId(2), KindId::browser()),
            ],
        );
        let (new_tree, map) = tree.rebase_slot_ids(100);
        assert_eq!(new_tree.slot_ids(), vec![SlotId(100), SlotId(101)]);
        assert_eq!(map.get(&SlotId(1)), Some(&SlotId(100)));
        assert_eq!(map.get(&SlotId(2)), Some(&SlotId(101)));
    }

    /// Rebasing cannot change the SHAPE: same tree, same kinds, same
    /// sizes. Only the numbers.
    #[test]
    fn rebase_preserves_the_shape() {
        let tree = Node::split(
            Dir::Vertical,
            vec![
                Node::slot(SlotId(3), KindId::new("places")),
                Node::split(
                    Dir::Horizontal,
                    vec![
                        Node::slot(SlotId(1), KindId::browser()),
                        Node::slot(SlotId(2), KindId::new("viewer")),
                    ],
                ),
            ],
        );
        let (new_tree, _) = tree.rebase_slot_ids(50);
        assert_eq!(new_tree.slot_ids().len(), tree.slot_ids().len());
        assert!(crate::layout::validate(&new_tree).is_ok());
    }

    /// A healthy tree, rebased, cannot MANUFACTURE a duplicate, whatever
    /// the order of the original ids.
    #[test]
    fn rebase_no_factory_duplicates() {
        let tree = Node::split(
            Dir::Horizontal,
            vec![
                Node::slot(SlotId(7), KindId::browser()),
                Node::slot(SlotId(1), KindId::browser()),
            ],
        );
        let (new_tree, _) = tree.rebase_slot_ids(10);
        assert!(new_tree.duplicate_slot_ids().is_empty());
    }

    /// The tree round-trips: it is the SAME format as the config file, the
    /// L2 session blob and what the layout editor will spit out. A format
    /// that does not round-trip forces migrating between two, which is
    /// exactly what ADR 0058 avoids.
    #[test]
    fn the_tree_round_trips() {
        let tree = Node::Split {
            dir: Dir::Horizontal,
            sizes: vec![Size::Weight(1), Size::Weight(1)],
            children: vec![
                Node::slot(SlotId(1), KindId::browser()),
                Node::Tabs {
                    active: 1,
                    children: vec![
                        Node::slot(SlotId(2), KindId::browser()),
                        Node::slot(SlotId(3), KindId::new("viewer")),
                    ],
                },
            ],
        };
        let json = serde_json::to_string(&tree).expect("serializes");
        assert_eq!(
            serde_json::from_str::<Node>(&json).expect("comes back"),
            tree
        );
    }

    /// An UNKNOWN kind survives the round-trip with its `params` intact.
    /// It is the model's rule 3: a client that does not know how to paint a
    /// kind must not erase it from the OTHER's layout.
    #[test]
    fn an_unknown_kind_preserves_its_params() {
        let json = r#"{"slot":{"id":7,"kind":"terminal","params":{"shell":"fish"},"bindings":{}}}"#;
        let n: Node = serde_json::from_str(json).expect("a kind we do not know parses");
        let round_trip = serde_json::to_string(&n).expect("serializes");
        assert!(
            round_trip.contains("\"shell\":\"fish\""),
            "the params get lost: {round_trip}"
        );
    }

    /// A tree's visible ids, in reading order. Used by roles, store and
    /// resolve, so it is tested here once.
    #[test]
    fn slot_ids_also_walks_hidden_tabs() {
        let tree = Node::Tabs {
            active: 0,
            children: vec![
                Node::slot(SlotId(1), KindId::browser()),
                Node::slot(SlotId(2), KindId::browser()),
            ],
        };
        assert_eq!(tree.slot_ids(), vec![SlotId(1), SlotId(2)]);
    }

    /// `substitute_auto` changes the `Auto`s to `Fixed` and touches NOTHING
    /// else: the saved tree keeps its `Auto`s, the frame's copy does not
    /// have them.
    #[test]
    fn substitute_auto_only_changes_the_autos() {
        let tree = Node::Split {
            dir: Dir::Vertical,
            sizes: vec![Size::Weight(1), Size::Auto, Size::Fixed(1)],
            children: vec![
                Node::slot(SlotId(1), KindId::browser()),
                Node::slot(SlotId(2), KindId::new("tasks")),
                Node::slot(SlotId(3), KindId::new("status")),
            ],
        };
        let from_frame = tree.substitute_auto(&|id| if id == SlotId(2) { (0, 4) } else { (0, 0) });
        let Node::Split { sizes, .. } = &from_frame else {
            panic!("still a split")
        };
        assert_eq!(
            *sizes,
            vec![Size::Weight(1), Size::Fixed(4), Size::Fixed(1)]
        );
        let Node::Split { sizes: orig, .. } = &tree else {
            panic!("split")
        };
        assert_eq!(orig[1], Size::Auto, "the saved tree is untouched");
    }

    /// On a HORIZONTAL cut, `Auto` takes the natural WIDTH, not the height.
    /// A sidebar measures however wide it is; its height is set by the
    /// distribution.
    #[test]
    fn substitute_auto_takes_the_cuts_axis() {
        let tree = Node::Split {
            dir: Dir::Horizontal,
            sizes: vec![Size::Auto, Size::Weight(1)],
            children: vec![
                Node::slot(SlotId(1), KindId::new("places")),
                Node::slot(SlotId(2), KindId::browser()),
            ],
        };
        let from_frame = tree.substitute_auto(&|_| (18, 3));
        let Node::Split { sizes, .. } = &from_frame else {
            panic!("split")
        };
        assert_eq!(sizes[0], Size::Fixed(18), "the width, not the height");
    }

    /// A subtree's `Auto` is based on its FIRST slot: it is the only one
    /// that does not depend on how things get distributed afterward.
    #[test]
    fn the_first_slot_is_the_one_that_gets_asked() {
        let tree = Node::split(
            Dir::Horizontal,
            vec![
                Node::slot(SlotId(7), KindId::browser()),
                Node::slot(SlotId(8), KindId::browser()),
            ],
        );
        assert_eq!(tree.first_slot_id(), Some(SlotId(7)));
    }

    fn b(id: u32) -> Node {
        Node::slot(SlotId(id), KindId::browser())
    }

    /// Opening a tab from a loose panel wraps it and leaves the new one
    /// active. Requiring two steps for that would make no sense.
    #[test]
    fn opening_a_tab_from_a_loose_pane_wraps_it() {
        let tree = Node::split(Dir::Horizontal, vec![b(1), b(2)]);
        let new_tree = tree.add_tab(SlotId(2), &b(9));
        assert_eq!(new_tree.slot_ids(), vec![SlotId(1), SlotId(2), SlotId(9)]);
        assert_eq!(
            new_tree.tabs_of(SlotId(2)),
            Some((vec![SlotId(2), SlotId(9)], 1))
        );
    }

    /// The `Tabs` that wins is the INNER one, not the one wrapping half the
    /// screen.
    #[test]
    fn a_new_tab_enters_the_innermost_group() {
        let tree = Node::Tabs {
            children: vec![Node::split(
                Dir::Horizontal,
                vec![
                    b(1),
                    Node::Tabs {
                        children: vec![b(2)],
                        active: 0,
                    },
                ],
            )],
            active: 0,
        };
        let new_tree = tree.add_tab(SlotId(2), &b(9));
        assert_eq!(
            new_tree.tabs_of(SlotId(2)),
            Some((vec![SlotId(2), SlotId(9)], 1))
        );
        // The outer group is still down to a single tab.
        assert_eq!(new_tree.tabs_of(SlotId(1)), Some((vec![SlotId(1)], 0)));
    }

    /// A `Tabs` left with ONE child DISSOLVES: a one-tab group is not a
    /// group, and leaving it would paint a bar with a single entry
    /// forever.
    #[test]
    fn closing_the_second_to_last_tab_dissolves_the_group() {
        let tree = Node::Tabs {
            children: vec![b(1), b(2)],
            active: 1,
        };
        let new_tree = tree.close_tab(SlotId(2)).expect("was in a group");
        assert_eq!(new_tree, b(1), "the group disappears and the slot remains");
    }

    /// Closing a LOOSE panel is not `pane.tab-close`: it returns `None` and
    /// the caller decides (it will be `layout.close-slot`).
    #[test]
    fn closing_a_loose_pane_is_not_closing_a_tab() {
        let tree = Node::split(Dir::Horizontal, vec![b(1), b(2)]);
        assert_eq!(tree.close_tab(SlotId(2)), None);
    }

    /// Closing a tab before the active one drags the active index along:
    /// otherwise, the active one would end up naming the tab next to it.
    #[test]
    fn closing_an_earlier_tab_drags_the_active_one() {
        let tree = Node::Tabs {
            children: vec![b(1), b(2), b(3)],
            active: 2,
        };
        let new_tree = tree.close_tab(SlotId(1)).expect("is in the group");
        assert_eq!(
            new_tree.tabs_of(SlotId(3)),
            Some((vec![SlotId(2), SlotId(3)], 1))
        );
    }

    /// Moving a tab takes the active one along with it.
    #[test]
    fn moving_a_tab_takes_the_active_one_with_it() {
        let tree = Node::Tabs {
            children: vec![b(1), b(2), b(3)],
            active: 0,
        };
        let new_tree = tree.move_tab(SlotId(1), 2);
        assert_eq!(
            new_tree.tabs_of(SlotId(1)),
            Some((vec![SlotId(2), SlotId(3), SlotId(1)], 2))
        );
    }

    /// Moving past the border stays at the border, it does not wrap
    /// around: a tab that jumps from last to first on one keystroke too
    /// many is exactly what nobody wanted.
    #[test]
    fn moving_a_tab_does_not_flip_it_over() {
        let tree = Node::Tabs {
            children: vec![b(1), b(2)],
            active: 1,
        };
        let new_tree = tree.move_tab(SlotId(2), 5);
        assert_eq!(
            new_tree.tabs_of(SlotId(2)),
            Some((vec![SlotId(1), SlotId(2)], 1))
        );
    }

    /// Splitting a slot leaves it with the new one alongside, both at the
    /// same weight.
    #[test]
    fn splitting_a_slot_leaves_both_at_the_same_weight() {
        let tree = b(1);
        let new_tree = tree.split_slot(SlotId(1), Dir::Vertical, &b(9));
        assert_eq!(new_tree.slot_ids(), vec![SlotId(1), SlotId(9)]);
        let Node::Split { dir, sizes, .. } = &new_tree else {
            panic!("split")
        };
        assert_eq!(*dir, Dir::Vertical);
        assert_eq!(*sizes, vec![Size::Weight(1), Size::Weight(1)]);
    }

    /// Splitting AGAIN on the same axis gives THIRDS, not a quarter.
    ///
    /// The cut joins the `Split` already running on that axis instead of
    /// wrapping the slot in a new one. Nesting, each split used to take
    /// half of the half: three panels ended up at 1/2, 1/4 and 1/4, and on
    /// the fourth the deepest child dropped below the kind's minimum and
    /// the distribution demoted it to tabs — the panel just requested
    /// vanished without a word.
    #[test]
    fn splitting_on_the_same_axis_divides_evenly() {
        let tree = b(1).split_slot(SlotId(1), Dir::Vertical, &b(2));
        let triple = tree.split_slot(SlotId(2), Dir::Vertical, &b(3));
        let Node::Split {
            children, sizes, ..
        } = &triple
        else {
            panic!("split")
        };
        assert_eq!(children.len(), 3, "a single Split with three children");
        assert_eq!(
            *sizes,
            vec![Size::Weight(1), Size::Weight(1), Size::Weight(1)]
        );
        assert_eq!(
            triple.slot_ids(),
            vec![SlotId(1), SlotId(2), SlotId(3)],
            "and the new one enters RIGHT NEXT to the one that split, not at the end"
        );
    }

    /// On the OTHER axis it still wraps: a perpendicular cut cannot enter
    /// its siblings' row.
    #[test]
    fn splitting_on_the_other_axis_keeps_nesting() {
        let tree = b(1).split_slot(SlotId(1), Dir::Vertical, &b(2));
        let cross = tree.split_slot(SlotId(2), Dir::Horizontal, &b(3));
        let Node::Split { children, dir, .. } = &cross else {
            panic!("split")
        };
        assert_eq!(*dir, Dir::Vertical);
        assert_eq!(children.len(), 2, "the outer one still has two children");
        assert!(
            matches!(&children[1], Node::Split { dir, .. } if *dir == Dir::Horizontal),
            "and the new cut goes INSIDE the one that split"
        );
    }

    /// A FIXED-size slot splits by wrapping, it does not join its siblings:
    /// its size is coupled chrome, and putting another child into that row
    /// would steal the room from what is alongside it.
    #[test]
    fn splitting_a_fixed_slot_does_not_merge_with_its_siblings() {
        let tree = Node::Split {
            dir: Dir::Vertical,
            sizes: vec![Size::Weight(1), Size::Fixed(8)],
            children: vec![b(1), b(2)],
        };
        let split_tree = tree.split_slot(SlotId(2), Dir::Vertical, &b(3));
        let Node::Split {
            children, sizes, ..
        } = &split_tree
        else {
            panic!("split")
        };
        assert_eq!(children.len(), 2, "there are still two children up top");
        assert_eq!(sizes[1], Size::Fixed(8), "and the fixed one keeps its size");
        assert_eq!(children[1].slot_ids(), vec![SlotId(2), SlotId(3)]);
    }

    /// Splitting a TAB splits what you are looking at, it does not
    /// reorganize its siblings: the cut goes inside the tab, not around
    /// the group.
    #[test]
    fn splitting_a_tab_cuts_inside_it() {
        let tree = Node::Tabs {
            children: vec![b(1), b(2)],
            active: 1,
        };
        let new_tree = tree.split_slot(SlotId(2), Dir::Horizontal, &b(9));
        let Node::Tabs { children, active } = &new_tree else {
            panic!("still a tab group")
        };
        assert_eq!(*active, 1, "the active tab does not move");
        assert_eq!(children.len(), 2, "there are still TWO tabs");
        assert_eq!(children[1].slot_ids(), vec![SlotId(2), SlotId(9)]);
    }

    #[test]
    fn the_tree_round_trips_in_toml() {
        use crate::layout::{Dir, KindId, Node, Size, SlotId};
        let tree = Node::Split {
            dir: Dir::Vertical,
            sizes: vec![Size::Weight(1), Size::Auto, Size::Fixed(1)],
            children: vec![
                Node::split(
                    Dir::Horizontal,
                    vec![
                        Node::slot(SlotId(1), KindId::browser()),
                        Node::slot(SlotId(2), KindId::browser()),
                    ],
                ),
                Node::slot(SlotId(3), KindId::new("tasks")),
                Node::slot(SlotId(4), KindId::new("status")),
            ],
        };
        let t = toml::to_string_pretty(&tree).expect("serializes to TOML");
        println!("---\n{t}\n---");
        let round_trip: Node = toml::from_str(&t).expect("comes back from TOML");
        assert_eq!(round_trip, tree);
    }

    /// Closing a slot leaves its sibling occupying both slots' spot.
    #[test]
    fn closing_a_slot_dissolves_the_split_of_two() {
        let tree = Node::split(Dir::Horizontal, vec![b(1), b(2)]);
        assert_eq!(tree.close_slot(SlotId(2)), Some(b(1)));
    }

    /// Closing the ONLY slot returns `None`: a screen with nothing in it is
    /// not the tree's call to make.
    #[test]
    fn closing_the_only_slot_does_not_happen_by_itself() {
        assert_eq!(b(1).close_slot(SlotId(1)), None);
    }

    /// On closing, the slot's size goes WITH it: leaving it would shift
    /// every weight one position over and the distribution would end up
    /// different without warning.
    #[test]
    fn closing_a_slot_takes_its_size_with_it() {
        let tree = Node::Split {
            dir: Dir::Horizontal,
            sizes: vec![Size::Weight(3), Size::Weight(1), Size::Weight(1)],
            children: vec![b(1), b(2), b(3)],
        };
        let new_tree = tree.close_slot(SlotId(1)).expect("two are left");
        let Node::Split { sizes, .. } = &new_tree else {
            panic!("still a split")
        };
        assert_eq!(*sizes, vec![Size::Weight(1), Size::Weight(1)]);
    }

    /// Growing touches the focused slot's weight, up to a cap.
    #[test]
    fn growing_raises_the_weight_up_to_the_cap() {
        let tree = Node::split(Dir::Horizontal, vec![b(1), b(2)]);
        let mut a = tree;
        for _ in 0..20 {
            a = a.resize(SlotId(1), 1);
        }
        let Node::Split { sizes, .. } = &a else {
            panic!("split")
        };
        assert_eq!(sizes[0], Size::Weight(10), "does not grow without limit");
    }

    /// A FIXED child DOES grow since #227, in cells: that was what left the
    /// sidebar stuck at the width it opened with.
    ///
    /// What protects the status bar — the other fixed child on screen — is
    /// not this function: it is that `layout.grow` only names the slot
    /// WITH FOCUS, and the `status` kind is not focusable. A cap here based
    /// on the child's size would be guessing which of the two fixed ones is
    /// a sidebar.
    #[test]
    fn growing_moves_a_fixed_child_in_cells() {
        let tree = Node::Split {
            dir: Dir::Vertical,
            sizes: vec![Size::Weight(1), Size::Fixed(1)],
            children: vec![b(1), Node::slot(SlotId(2), KindId::new("status"))],
        };
        let new_tree = tree.resize(SlotId(2), 3);
        let Node::Split { sizes, .. } = &new_tree else {
            panic!("split")
        };
        assert_eq!(sizes[1], Size::Fixed(7));
    }

    /// Equalizing returns the weights to one and leaves the fixed ones
    /// alone.
    #[test]
    fn equalizing_only_touches_the_weighted_ones() {
        let tree = Node::Split {
            dir: Dir::Vertical,
            sizes: vec![Size::Weight(7), Size::Fixed(2), Size::Weight(3)],
            children: vec![b(1), b(2), b(3)],
        };
        let new_tree = tree.equalize(SlotId(1));
        let Node::Split { sizes, .. } = &new_tree else {
            panic!("split")
        };
        assert_eq!(
            *sizes,
            vec![Size::Weight(1), Size::Fixed(2), Size::Weight(1)]
        );
    }

    /// Two slots with the same id is incoherent, and the tree knows how to
    /// say so.
    #[test]
    fn repeated_ids_are_detected() {
        let tree = Node::Split {
            dir: Dir::Vertical,
            sizes: vec![Size::Weight(1), Size::Weight(1)],
            children: vec![
                Node::slot(SlotId(1), KindId::browser()),
                Node::slot(SlotId(1), KindId::browser()),
            ],
        };
        assert_eq!(tree.duplicate_slot_ids(), vec![SlotId(1)]);
    }

    /// A dock to the left enters the HORIZONTAL `Split` that already
    /// exists, not around the whole tree: if it wrapped the root, the
    /// status bar and the task strip would end up to the RIGHT of the
    /// sidebar instead of below the listings.
    #[test]
    fn dock_left_enters_the_body_split() {
        let body = Node::split(
            Dir::Horizontal,
            vec![
                Node::slot(SlotId(1), KindId::browser()),
                Node::slot(SlotId(2), KindId::browser()),
            ],
        );
        let root = Node::Split {
            dir: Dir::Vertical,
            sizes: vec![Size::Weight(1), Size::Fixed(1)],
            children: vec![body, Node::slot(SlotId(4), KindId::new("status"))],
        };
        let docked = root.dock(
            SlotId(1),
            Edge::Left,
            Size::Fixed(16),
            &Node::slot(SlotId(9), KindId::new("places")),
        );
        let Node::Split { children, .. } = &docked else {
            panic!("the root is still a Split");
        };
        let Node::Split {
            children: body,
            sizes,
            dir,
        } = &children[0]
        else {
            panic!("the body is still a Split");
        };
        assert_eq!(*dir, Dir::Horizontal);
        assert_eq!(body.len(), 3);
        assert_eq!(body[0].first_slot_id(), Some(SlotId(9)));
        assert_eq!(sizes[0], Size::Fixed(16));
        // And the status bar did NOT move: it is still a child of the root.
        assert_eq!(children[1].first_slot_id(), Some(SlotId(4)));
    }

    /// To the right, at the end of the same split.
    #[test]
    fn right_dock_goes_last() {
        let tree = Node::split(
            Dir::Horizontal,
            vec![
                Node::slot(SlotId(1), KindId::browser()),
                Node::slot(SlotId(2), KindId::browser()),
            ],
        );
        let docked = tree.dock(
            SlotId(1),
            Edge::Right,
            Size::Weight(1),
            &Node::slot(SlotId(9), KindId::new("viewer")),
        );
        let Node::Split { children, .. } = &docked else {
            panic!("split")
        };
        assert_eq!(children.len(), 3);
        assert_eq!(children[2].first_slot_id(), Some(SlotId(9)));
    }

    /// REGRESSION (2026-09-21 capture): a WEIGHT is measured against its
    /// sibling weights, never in absolute terms. Dragging the border
    /// between two listings leaves them at 49/51; a viewer entering with
    /// `Weight(1)` used to get 1/101 of the free space — a one-pixel
    /// sliver you cannot see. It enters with the AVERAGE of its siblings'
    /// weights, which is what `Weight(1)` means in a distribution of ones.
    #[test]
    fn an_incoming_weight_is_measured_against_its_siblings() {
        let tree = Node::Split {
            dir: Dir::Horizontal,
            children: vec![
                Node::slot(SlotId(5), KindId::new("places")),
                Node::slot(SlotId(1), KindId::browser()),
                Node::slot(SlotId(2), KindId::browser()),
            ],
            sizes: vec![Size::Fixed(16), Size::Weight(49), Size::Weight(51)],
        };
        let docked = tree.dock(
            SlotId(1),
            Edge::Right,
            Size::Weight(1),
            &Node::slot(SlotId(9), KindId::new("viewer")),
        );
        let Node::Split { sizes, .. } = &docked else {
            panic!("split")
        };
        assert_eq!(sizes.last(), Some(&Size::Weight(50)), "{sizes:?}");
        // A fixed one is untouched: its number is cells, not proportion.
        let docked = tree.dock(
            SlotId(1),
            Edge::Right,
            Size::Fixed(30),
            &Node::slot(SlotId(9), KindId::new("metadata")),
        );
        let Node::Split { sizes, .. } = &docked else {
            panic!("split")
        };
        assert_eq!(sizes.last(), Some(&Size::Fixed(30)));
    }

    /// A panel docked at the BOTTOM enters ABOVE the task strip and the
    /// status bar, not below (2026-09-21 capture): the log and processes
    /// used to come out below the status bar. In VS Code the bottom panel
    /// is always above the bar; and in the terminal, the status bar has to
    /// be the last row.
    #[test]
    fn below_enters_above_the_status_bar() {
        let tree = Node::Split {
            dir: Dir::Vertical,
            children: vec![
                Node::slot(SlotId(1), KindId::browser()),
                Node::slot(SlotId(3), KindId::new("tasks")),
                Node::slot(SlotId(4), KindId::new("status")),
            ],
            sizes: vec![Size::Weight(1), Size::Auto, Size::Fixed(1)],
        };
        let docked = tree.dock(
            SlotId(1),
            Edge::Bottom,
            Size::Fixed(12),
            &Node::slot(SlotId(9), KindId::new("log")),
        );
        let Node::Split {
            children, sizes, ..
        } = &docked
        else {
            panic!("split")
        };
        let ids: Vec<_> = children.iter().filter_map(Node::first_slot_id).collect();
        assert_eq!(ids, [SlotId(1), SlotId(9), SlotId(3), SlotId(4)]);
        assert_eq!(sizes[1], Size::Fixed(12), "the size travels with its child");
    }

    /// Panels on the same edge get GROUPED into tabs (spec 2026-09-21,
    /// phase F): a second panel docked to the right does not open another
    /// column, it joins the first one's and lands in front; the size is
    /// the group's. A listing never groups, and closing a tab dissolves
    /// the group.
    #[test]
    fn panes_on_the_same_edge_group_into_tabs() {
        let body = Node::Split {
            dir: Dir::Horizontal,
            children: vec![
                Node::slot(SlotId(1), KindId::browser()),
                Node::slot(SlotId(2), KindId::browser()),
            ],
            sizes: vec![Size::Weight(1), Size::Weight(1)],
        };
        let leaf = |id: u32, k: &str| Node::slot(SlotId(id), KindId::new(k));
        // The first one: a column, as always (its neighbor is a listing).
        let one = body.dock_grouped(
            SlotId(1),
            Edge::Right,
            Size::Fixed(30),
            &leaf(7, "timeline"),
        );
        let Node::Split { children, .. } = &one else {
            panic!("split")
        };
        assert_eq!(children.len(), 3);
        // The second one: a tab of the first, in front, and the group
        // takes the room of whichever asks for more — the viewer is
        // proportional, and it would not read in the first one's thirty
        // columns.
        let two = one.dock_grouped(SlotId(1), Edge::Right, Size::Weight(1), &leaf(9, "viewer"));
        let Node::Split {
            children, sizes, ..
        } = &two
        else {
            panic!("split")
        };
        assert_eq!(children.len(), 3, "does not open another column");
        assert_eq!(
            two.tabs_of(SlotId(9)),
            Some((vec![SlotId(7), SlotId(9)], 1))
        );
        assert_eq!(sizes[2], Size::Weight(1));
        // The third one joins the same group, and a fixed one does not
        // steal its weight.
        let three = two.dock_grouped(
            SlotId(1),
            Edge::Right,
            Size::Fixed(30),
            &leaf(8, "metadata"),
        );
        assert_eq!(
            three.tabs_of(SlotId(8)),
            Some((vec![SlotId(7), SlotId(9), SlotId(8)], 2))
        );
        // At the bottom, above the status bar, the same.
        let root = Node::Split {
            dir: Dir::Vertical,
            children: vec![three, leaf(4, "status")],
            sizes: vec![Size::Weight(1), Size::Fixed(1)],
        };
        let r = root
            .dock_grouped(SlotId(1), Edge::Bottom, Size::Fixed(12), &leaf(10, "log"))
            .dock_grouped(
                SlotId(1),
                Edge::Bottom,
                Size::Fixed(8),
                &leaf(11, "processes"),
            );
        assert_eq!(
            r.tabs_of(SlotId(11)),
            Some((vec![SlotId(10), SlotId(11)], 1))
        );
        // Two fixed ones: the larger, so the log does not shrink to the
        // processes' eight rows.
        let Node::Split { sizes, .. } = &r else {
            panic!("split")
        };
        assert_eq!(sizes[1], Size::Fixed(12));
        // Closing a tab from a group of two dissolves it: the leaf comes
        // back.
        let closed = r.close_slot(SlotId(11)).expect("closable");
        assert_eq!(closed.tabs_of(SlotId(10)), None);
        // And plain `dock` still does not group: it is the one for
        // hand-written layouts and the presets.
        let loose = two.dock(
            SlotId(1),
            Edge::Right,
            Size::Fixed(30),
            &leaf(8, "metadata"),
        );
        assert_eq!(loose.tabs_of(SlotId(8)), None);
    }

    /// With no ancestor on the requested axis, it WRAPS. A single pane is
    /// the real case: after closing one, the body can be a loose leaf.
    #[test]
    fn without_ancestor_on_axis_it_wraps() {
        let tree = Node::slot(SlotId(1), KindId::browser());
        let docked = tree.dock(
            SlotId(1),
            Edge::Left,
            Size::Fixed(16),
            &Node::slot(SlotId(9), KindId::new("places")),
        );
        let Node::Split {
            dir,
            children,
            sizes,
        } = &docked
        else {
            panic!("wrapped in a Split")
        };
        assert_eq!(*dir, Dir::Horizontal);
        assert_eq!(children[0].first_slot_id(), Some(SlotId(9)));
        assert_eq!(children[1].first_slot_id(), Some(SlotId(1)));
        assert_eq!(sizes, &vec![Size::Fixed(16), Size::Weight(1)]);
    }

    /// An anchor INSIDE a `Tabs` docks OUTSIDE the group: a sidebar that
    /// disappears when the tab changes is not a sidebar.
    #[test]
    fn with_the_anchor_on_a_tab_the_docking_goes_outside_the_group() {
        let tree = Node::split(
            Dir::Horizontal,
            vec![
                Node::Tabs {
                    children: vec![
                        Node::slot(SlotId(1), KindId::browser()),
                        Node::slot(SlotId(3), KindId::browser()),
                    ],
                    active: 0,
                },
                Node::slot(SlotId(2), KindId::browser()),
            ],
        );
        let docked = tree.dock(
            SlotId(1),
            Edge::Left,
            Size::Fixed(16),
            &Node::slot(SlotId(9), KindId::new("places")),
        );
        let Node::Split { children, .. } = &docked else {
            panic!("split")
        };
        assert_eq!(children.len(), 3);
        assert_eq!(children[0].first_slot_id(), Some(SlotId(9)));
        assert!(matches!(children[1], Node::Tabs { .. }));
    }

    /// An anchor that is not in the tree invents nothing.
    #[test]
    fn an_anchor_that_does_not_exist_leaves_the_tree_intact() {
        let tree = Node::slot(SlotId(1), KindId::browser());
        let docked = tree.dock(
            SlotId(77),
            Edge::Left,
            Size::Fixed(16),
            &Node::slot(SlotId(9), KindId::new("places")),
        );
        assert_eq!(docked, tree);
    }

    /// And undoing it is `close_slot`, which already exists: the
    /// single-child `Split` dissolves and the tree goes back to what it
    /// was before. It is what makes the toggle genuinely reversible and
    /// keeps it from leaving a degenerate Split every time someone opened
    /// and closed the sidebar.
    #[test]
    fn undock_is_close_slot_and_returns_the_previous_tree() {
        let tree = Node::split(
            Dir::Horizontal,
            vec![
                Node::slot(SlotId(1), KindId::browser()),
                Node::slot(SlotId(2), KindId::browser()),
            ],
        );
        let docked = tree.dock(
            SlotId(1),
            Edge::Left,
            Size::Fixed(16),
            &Node::slot(SlotId(9), KindId::new("places")),
        );
        assert_eq!(docked.close_slot(SlotId(9)), Some(tree));
    }

    /// A `Split`'s first child's width, for the `resize` tests.
    fn width(n: &Node) -> Size {
        match n {
            Node::Split { sizes, .. } => sizes[0],
            _ => panic!("split"),
        }
    }

    fn with_sidebar(width: u16) -> Node {
        Node::Split {
            dir: Dir::Horizontal,
            children: vec![
                Node::slot(SlotId(5), KindId::new("places")),
                Node::slot(SlotId(1), KindId::browser()),
            ],
            sizes: vec![Size::Fixed(width), Size::Weight(1)],
        }
    }

    /// #227: a FIXED child — the sidebar's width — moves in CELLS. `resize`
    /// used to only touch weights, so the places panel could not be
    /// widened with the keyboard and presets with a sidebar were born
    /// stuck.
    #[test]
    fn a_fixed_child_moves_in_cells() {
        let tree = with_sidebar(16);
        assert_eq!(width(&tree.resize(SlotId(5), 1)), Size::Fixed(18));
        assert_eq!(width(&tree.resize(SlotId(5), -1)), Size::Fixed(14));
    }

    /// The lower cap exists so it cannot be dropped to zero: a panel of
    /// zero width is invisible and there is no way to grow it back.
    #[test]
    fn a_fixed_child_does_not_go_below_two_nor_above_a_hundred() {
        assert_eq!(
            width(&with_sidebar(2).resize(SlotId(5), -1)),
            Size::Fixed(2)
        );
        assert_eq!(
            width(&with_sidebar(100).resize(SlotId(5), 1)),
            Size::Fixed(100)
        );
    }

    /// And a WEIGHTED child keeps doing exactly what it did before.
    #[test]
    fn a_weighted_child_does_not_change_behavior() {
        let tree = Node::split(
            Dir::Horizontal,
            vec![
                Node::slot(SlotId(1), KindId::browser()),
                Node::slot(SlotId(2), KindId::browser()),
            ],
        );
        assert_eq!(width(&tree.resize(SlotId(1), 1)), Size::Weight(2));
        assert_eq!(width(&tree.resize(SlotId(1), -1)), Size::Weight(1));
    }

    /// The usual `orthodox`: two listings side by side over the chrome
    /// rows.
    fn orthodox() -> Node {
        Node::Split {
            dir: Dir::Vertical,
            children: vec![
                Node::split(Dir::Horizontal, vec![b(1), b(2)]),
                Node::slot(SlotId(3), KindId::new("tasks")),
                Node::slot(SlotId(4), KindId::new("status")),
            ],
            sizes: vec![Size::Weight(1), Size::Auto, Size::Fixed(1)],
        }
    }

    /// ADR 0138: dropping to a side splits with the destination evenly, and
    /// in the center it joins as a tab. Nothing is lost.
    #[test]
    fn moving_a_slot_drops_it_beside_or_as_a_tab() {
        let t = orthodox();
        // 1 below 2: the horizontal split dissolves, and 2 splits
        // vertically INSIDE its own spot — not as a sibling at the root,
        // which is the chrome's and does not rotate: that is how `flip`
        // undoes it.
        let below = t.move_slot(SlotId(1), SlotId(2), DropZone::Bottom);
        let Node::Split { children, .. } = &below else {
            panic!("root")
        };
        assert_eq!(
            children[0],
            Node::Split {
                dir: Dir::Vertical,
                children: vec![b(2), b(1)],
                sizes: vec![Size::Weight(1); 2],
            }
        );
        assert_eq!(children.len(), 3, "the chrome is unchanged");
        assert_eq!(
            below.flip(SlotId(1)),
            t.move_slot(SlotId(1), SlotId(2), DropZone::Right),
            "flipping what was dropped below is dropping it to the right"
        );
        // A third one to the right of 1 enters as a SIBLING: thirds.
        let three = Node::split(Dir::Horizontal, vec![b(1), b(2), b(5)]);
        let moved = three.move_slot(SlotId(5), SlotId(1), DropZone::Right);
        let Node::Split {
            children, sizes, ..
        } = &moved
        else {
            panic!("split")
        };
        assert_eq!(children, &vec![b(1), b(5), b(2)]);
        assert_eq!(sizes, &vec![Size::Weight(1); 3]);
        // In the center: a tab of the destination, in front.
        let center = t.move_slot(SlotId(1), SlotId(2), DropZone::Center);
        assert_eq!(
            center.tabs_of(SlotId(1)),
            Some((vec![SlotId(2), SlotId(1)], 1))
        );
        // The same slots, always.
        for m in [&below, &moved, &center] {
            let mut ids = m.slot_ids();
            ids.sort_unstable();
            assert!(m.duplicate_slot_ids().is_empty());
            assert!(ids.windows(2).all(|w| w[0] != w[1]));
        }
    }

    /// Dropping next to a tab splits the GROUP, it does not invade it.
    #[test]
    fn dropping_next_to_a_tab_splits_its_group() {
        let group = Node::Tabs {
            children: vec![b(1), b(2)],
            active: 0,
        };
        let t = Node::split(Dir::Horizontal, vec![group.clone(), b(3)]);
        let m = t.move_slot(SlotId(3), SlotId(2), DropZone::Top);
        assert_eq!(
            m,
            Node::Split {
                dir: Dir::Vertical,
                children: vec![b(3), group],
                sizes: vec![Size::Weight(1); 2],
            }
        );
    }

    /// What does not move: onto itself, the chrome, the only slot, an id
    /// that is not there.
    #[test]
    fn moving_what_does_not_move_changes_nothing() {
        let t = orthodox();
        assert_eq!(t.move_slot(SlotId(1), SlotId(1), DropZone::Left), t);
        assert_eq!(t.move_slot(SlotId(4), SlotId(1), DropZone::Top), t);
        assert_eq!(t.move_slot(SlotId(1), SlotId(4), DropZone::Top), t);
        assert_eq!(t.move_slot(SlotId(9), SlotId(1), DropZone::Top), t);
        assert_eq!(b(1).move_slot(SlotId(1), SlotId(2), DropZone::Left), b(1));
    }

    /// The border between the second listing and the details separates the
    /// BODY from the details: the pair is measured whole, and dragging it
    /// moves the details even if the listing is the last one of its own
    /// distribution.
    #[test]
    fn the_edge_between_cousins_moves_its_pair() {
        let t = Node::Split {
            dir: Dir::Horizontal,
            children: vec![
                Node::split(Dir::Horizontal, vec![b(1), b(2)]),
                Node::slot(SlotId(7), KindId::new("metadata")),
            ],
            sizes: vec![Size::Weight(1), Size::Fixed(50)],
        };
        assert_eq!(
            t.border_pair(SlotId(2), SlotId(7)),
            Some((vec![SlotId(1), SlotId(2)], vec![SlotId(7)]))
        );
        assert_eq!(
            t.border_pair(SlotId(1), SlotId(2)),
            Some((vec![SlotId(1)], vec![SlotId(2)]))
        );
        assert_eq!(t.border_pair(SlotId(2), SlotId(1)), None, "order matters");
        // A 100-cell body and 50-cell details: the border at 120 leaves 30.
        let m = t.drag_border_between(SlotId(2), SlotId(7), 120.0 / 150.0, 150);
        let Node::Split { sizes, .. } = &m else {
            panic!("split")
        };
        assert_eq!(sizes[1], Size::Fixed(30));
    }

    /// Dragging the border between two weighted ones does not crush a
    /// third: the pair keeps its sum.
    #[test]
    fn dragging_a_pair_does_not_crush_the_third() {
        let t = Node::split(Dir::Horizontal, vec![b(1), b(2), b(3)]);
        let m = t.drag_border_between(SlotId(1), SlotId(2), 0.25, 60);
        let Node::Split { sizes, .. } = &m else {
            panic!("split")
        };
        let w: Vec<u16> = sizes
            .iter()
            .map(|s| match s {
                Size::Weight(w) => *w,
                _ => 0,
            })
            .collect();
        let total: u16 = w.iter().sum();
        assert_eq!(
            w[0] + w[1],
            w[2] * 2,
            "the pair still adds up to two thirds"
        );
        assert_eq!(w[2] * 3, total);
        assert!(w[0] < w[1]);
    }

    /// The zone under the pointer, in cells: the quarter rule, same as the
    /// window; and the part that gets highlighted.
    #[test]
    fn the_drop_zone_comes_from_the_nearest_quarter() {
        let r = Rect {
            x: 10,
            y: 0,
            width: 40,
            height: 20,
        };
        assert_eq!(DropZone::at(11, 10, r), DropZone::Left);
        assert_eq!(DropZone::at(48, 10, r), DropZone::Right);
        assert_eq!(DropZone::at(30, 1, r), DropZone::Top);
        assert_eq!(DropZone::at(30, 18, r), DropZone::Bottom);
        assert_eq!(DropZone::at(30, 10, r), DropZone::Center);
        assert_eq!(
            DropZone::Right.part_of(r),
            Rect {
                x: 30,
                y: 0,
                width: 20,
                height: 20
            }
        );
        assert_eq!(DropZone::Center.part_of(r), r);
    }

    /// Next to a fixed-width panel, what gets dropped enters as a SIBLING
    /// with weight: splitting the fixed one by wrapping would give a
    /// listing eight columns.
    #[test]
    fn dropping_next_to_a_fixed_one_enters_as_a_sibling() {
        let t = Node::Split {
            dir: Dir::Horizontal,
            children: vec![Node::slot(SlotId(7), KindId::new("places")), b(1), b(2)],
            sizes: vec![Size::Fixed(16), Size::Weight(1), Size::Weight(1)],
        };
        let m = t.move_slot(SlotId(2), SlotId(7), DropZone::Right);
        let Node::Split {
            sizes, children, ..
        } = &m
        else {
            panic!("split")
        };
        assert_eq!(
            sizes[0],
            Size::Fixed(16),
            "the places panel keeps its width"
        );
        assert_eq!(children[1], b(2));
        assert!(matches!(sizes[1], Size::Weight(_)));
    }

    /// The center only joins equal families (ADR 0134): a listing does not
    /// enter the places' tabs, nor a panel a listing's.
    #[test]
    fn the_center_does_not_mix_listings_and_panes() {
        let t = Node::Split {
            dir: Dir::Horizontal,
            children: vec![Node::slot(SlotId(7), KindId::new("places")), b(1), b(2)],
            sizes: vec![Size::Fixed(16), Size::Weight(1), Size::Weight(1)],
        };
        assert_eq!(t.move_slot(SlotId(1), SlotId(7), DropZone::Center), t);
        assert_eq!(t.move_slot(SlotId(7), SlotId(1), DropZone::Center), t);
        assert_ne!(t.move_slot(SlotId(1), SlotId(2), DropZone::Center), t);
    }

    /// ADR 0138: flipping turns side-by-side into one over the other and
    /// back; it does not rotate the chrome's distribution, and a fixed one
    /// turns into a weight.
    #[test]
    fn rotating_changes_the_inner_splits_axis() {
        let t = orthodox();
        let g = t.flip(SlotId(1));
        let Node::Split { children, .. } = &g else {
            panic!("root")
        };
        assert!(matches!(
            &children[0],
            Node::Split {
                dir: Dir::Vertical,
                ..
            }
        ));
        assert_eq!(g.flip(SlotId(2)), t, "flipping twice is not flipping");
        // A single listing over the chrome: nothing to flip.
        let single = Node::Split {
            dir: Dir::Vertical,
            children: vec![b(1), Node::slot(SlotId(4), KindId::new("status"))],
            sizes: vec![Size::Weight(1), Size::Fixed(1)],
        };
        assert_eq!(single.flip(SlotId(1)), single);
        // Only the WEIGHTED run: the places panel stays a sixteen-wide
        // column, and the two listings stack alongside it.
        let with_slots = Node::Split {
            dir: Dir::Horizontal,
            children: vec![Node::slot(SlotId(7), KindId::new("places")), b(1), b(2)],
            sizes: vec![Size::Fixed(16), Size::Weight(1), Size::Weight(1)],
        };
        let g = with_slots.flip(SlotId(1));
        assert_eq!(
            g,
            Node::Split {
                dir: Dir::Horizontal,
                children: vec![
                    Node::slot(SlotId(7), KindId::new("places")),
                    Node::split(Dir::Vertical, vec![b(1), b(2)]),
                ],
                sizes: vec![Size::Fixed(16), Size::Weight(2)],
            }
        );
        // And round-tripping: the places panel does not lose its width.
        let Node::Split { sizes, .. } = g.flip(SlotId(2)) else {
            panic!("split")
        };
        assert_eq!(sizes[0], Size::Fixed(16));
        // A run of one does not flip, and the negative case does NOT climb
        // up to flip the outer distribution.
        let lone_one = Node::Split {
            dir: Dir::Horizontal,
            children: vec![Node::slot(SlotId(7), KindId::new("places")), b(1)],
            sizes: vec![Size::Fixed(30), Size::Weight(1)],
        };
        assert_eq!(lone_one.flip(SlotId(1)), lone_one);
        let nested = Node::split(Dir::Vertical, vec![lone_one.clone(), b(9)]);
        assert_eq!(nested.flip(SlotId(1)), nested);
    }
}
