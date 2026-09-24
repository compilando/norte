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
fn es_cromo(n: &Node) -> bool {
    matches!(n, Node::Slot { kind, .. } if matches!(kind.as_str(), "status" | "tasks"))
}

/// A layout's sizes with the border between `pos` and `pos + 1` at fraction
/// `frac` of the `celdas` the two occupy (see [`Node::drag_border`]).
///
/// Two WEIGHTED ones keep their sum: what one gains the other loses, and
/// the rest of the layout never knows. Simply renormalizing the pair to a
/// hundred — what it used to do — left a third sibling of weight one
/// against a pair of a hundred: grabbing the border between two listings
/// crushed the one next to it. If the pair's sum is too small to have
/// granularity, the WHOLE layout is multiplied by the same factor, which
/// changes no proportion.
fn arrastrar_pareja(sizes: &[Size], pos: usize, frac: f32, celdas_del_par: u16) -> Vec<Size> {
    /// The pair's minimum weight for the drag to have granularity.
    const PESO_FINO: u32 = 100;
    /// The minimum left to each side, as a fraction.
    const MARGEN: f32 = 0.05;
    let frac = frac.clamp(MARGEN, 1.0 - MARGEN);
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
    let celdas = f32::from(celdas_del_par);
    // Rounding and clipping, in ONE place: `f32` to `u16` truncates and has
    // no sign, so the clamp goes before converting and not after — an `as`
    // on a negative or on 70000 gives no warning.
    let entero = |v: f32| -> u16 {
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
    let left_cells = (celdas * frac).round().clamp(1.0, (celdas - 1.0).max(1.0));
    match (ns[pos], next) {
        (Size::Fixed(_), Size::Fixed(_)) => {
            ns[pos] = Size::Fixed(entero(left_cells));
            ns[pos + 1] = Size::Fixed(entero(celdas - left_cells));
        }
        // A fixed one against a weighted one: the FIXED one is written and
        // the other keeps whatever is left, which is what layout already
        // did. Writing both would turn a weighted one into a fixed one by
        // dragging its border, and with that it would stop stretching when
        // the window resizes.
        (Size::Fixed(_), _) => ns[pos] = Size::Fixed(entero(left_cells)),
        (_, Size::Fixed(_)) => {
            ns[pos + 1] = Size::Fixed(entero(celdas - left_cells));
        }
        (Size::Weight(wa), Size::Weight(wb)) => {
            let sum = u32::from(wa) + u32::from(wb);
            let factor = PESO_FINO.div_ceil(sum.max(1)).max(1);
            if factor > 1 {
                for s in &mut ns {
                    if let Size::Weight(w) = s {
                        *w = u16::try_from(u32::from(*w) * factor).unwrap_or(u16::MAX);
                    }
                }
            }
            let sum = f32::from(u16::try_from(sum * factor).unwrap_or(u16::MAX));
            let left = (sum * frac).round().clamp(1.0, (sum - 1.0).max(1.0));
            ns[pos] = Size::Weight(entero(left));
            ns[pos + 1] = Size::Weight(entero(sum - left));
        }
        _ => {}
    }
    ns
}

/// What searching for what to flip returns (ADR 0138): three cases and not
/// an `Option`, because "not flipped here" has to STOP the search and "not
/// here" has to let it continue. With an `Option` the negative propagated
/// up and the outer layout got flipped.
enum Giro {
    /// Flipped: the new tree.
    Hecho(Node),
    /// Found, and not flipped.
    Rehusado,
    /// The slot is not in this subtree, or there is no layout to flip.
    NoEsta,
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
    /// same rule as `zonaDe` in the window (`render/mover.ts`).
    #[must_use]
    pub fn at(x: u16, y: u16, rect: Rect) -> Self {
        let frac = |p: u16, o: u16, largo: u16| {
            if largo == 0 {
                0.5
            } else {
                (f32::from(p.saturating_sub(o)) + 0.5) / f32::from(largo)
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
fn es_grupo_de_paneles(n: &Node) -> bool {
    match n {
        Node::Tabs { children, .. } => !children.is_empty() && children.iter().all(es_panel),
        otro => es_panel(otro),
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
fn peso_entre_hermanos(size: Size, hermanos: &[Size]) -> Size {
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
                let hijos: Vec<Self> = children
                    .iter()
                    .map(|c| c.substitute_auto(natural))
                    .collect();
                let nuevos = children
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
                        Some(otro) => *otro,
                        None => Size::Weight(1),
                    })
                    .collect();
                Self::Split {
                    dir: *dir,
                    children: hijos,
                    sizes: nuevos,
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

    /// Parte el hueco `id` en dos a lo largo de `dir`, con `nuevo` al lado.
    ///
    /// Los dos quedan con el mismo peso. Si `id` está dentro de una `Tabs`, el
    /// corte va DENTRO de esa pestaña y no alrededor del grupo: partir una
    /// pestaña es partir lo que estás mirando, no reorganizar sus hermanas.
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
    pub fn split_slot(&self, id: SlotId, dir: Dir, nuevo: &Self) -> Self {
        match self {
            Self::Slot { id: i, .. } if *i == id => {
                Self::split(dir, vec![self.clone(), nuevo.clone()])
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
                    && let Size::Weight(peso) = sizes.get(i).copied().unwrap_or(Size::Weight(1))
                {
                    let mut hijos = children.clone();
                    let mut tam = sizes.clone();
                    tam.resize(hijos.len(), Size::Weight(1));
                    hijos.insert(i + 1, nuevo.clone());
                    tam.insert(i + 1, Size::Weight(peso));
                    return Self::Split {
                        dir: *d,
                        children: hijos,
                        sizes: tam,
                    };
                }
                Self::Split {
                    dir: *d,
                    sizes: sizes.clone(),
                    children: children
                        .iter()
                        .map(|c| c.split_slot(id, dir, nuevo))
                        .collect(),
                }
            }
            Self::Tabs { children, active } => Self::Tabs {
                active: *active,
                children: children
                    .iter()
                    .map(|c| c.split_slot(id, dir, nuevo))
                    .collect(),
            },
        }
    }

    /// Docks `nuevo` against `edge` of the layout `anchor` lives in.
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
    pub fn dock(&self, anchor: SlotId, edge: Edge, size: Size, nuevo: &Self) -> Self {
        self.dock_con(anchor, edge, size, nuevo, false)
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
    pub fn dock_grouped(&self, anchor: SlotId, edge: Edge, size: Size, nuevo: &Self) -> Self {
        self.dock_con(anchor, edge, size, nuevo, true)
    }

    fn dock_con(
        &self,
        anchor: SlotId,
        edge: Edge,
        size: Size,
        nuevo: &Self,
        agrupar: bool,
    ) -> Self {
        if !self.contains(anchor) {
            return self.clone();
        }
        self.dock_inner(anchor, edge, size, nuevo, agrupar)
            .unwrap_or_else(|| {
                let (children, sizes) = if edge.is_front() {
                    (
                        vec![nuevo.clone(), self.clone()],
                        vec![size, Size::Weight(1)],
                    )
                } else {
                    (
                        vec![self.clone(), nuevo.clone()],
                        vec![Size::Weight(1), size],
                    )
                };
                Self::Split {
                    dir: edge.axis(),
                    children,
                    sizes,
                }
            })
    }

    /// `Some` if some `Split` on the path to `anchor` ran on the requested
    /// axis and took `nuevo`; `None` if none did, and then [`Self::dock`]
    /// decides.
    fn dock_inner(
        &self,
        anchor: SlotId,
        edge: Edge,
        size: Size,
        nuevo: &Self,
        agrupar: bool,
    ) -> Option<Self> {
        let hijos = match self {
            Self::Split { children, .. } | Self::Tabs { children, .. } => children,
            Self::Slot { .. } => return None,
        };
        let pos = hijos.iter().position(|c| c.contains(anchor))?;
        // Inward first: the layout that rules is the DEEPEST one that runs
        // on the axis, not the first one found going down.
        if let Some(dentro) = hijos[pos].dock_inner(anchor, edge, size, nuevo, agrupar) {
            return Some(self.with_child(pos, dentro));
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
        let vecino = if edge.is_front() {
            Some(0)
        } else {
            at.checked_sub(1)
        };
        if agrupar
            && es_panel(nuevo)
            && let Some(v) = vecino
            && nc.get(v).is_some_and(es_grupo_de_paneles)
        {
            let grupo = match &nc[v] {
                Self::Tabs { children, .. } => {
                    let mut h = children.clone();
                    h.push(nuevo.clone());
                    h
                }
                otro => vec![otro.clone(), nuevo.clone()],
            };
            let active = grupo.len() - 1;
            nc[v] = Self::Tabs {
                children: grupo,
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
                    (Size::Fixed(_), Size::Weight(_)) => peso_entre_hermanos(size, sizes),
                    _ => actual,
                };
            }
            return Some(Self::Split {
                dir: *dir,
                children: nc,
                sizes: ns,
            });
        }
        nc.insert(at, nuevo.clone());
        ns.insert(at.min(ns.len()), peso_entre_hermanos(size, sizes));
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
        let hijos = match self {
            Self::Split { children, .. } | Self::Tabs { children, .. } => children,
            Self::Slot { .. } => return None,
        };
        for (i, c) in hijos.iter().enumerate() {
            if let Some(cambiado) = c.close_slot(id) {
                return Some(self.with_child(i, cambiado));
            }
        }
        let pos = hijos.iter().position(|c| c.contains(id))?;
        if hijos.len() <= 1 {
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

    /// Moves slot `id` next to `target`: on its `zona` side, or as its tab
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
    pub fn move_slot(&self, id: SlotId, target: SlotId, zona: DropZone) -> Self {
        let movible = |s: SlotId| self.find_slot(s).is_some_and(|n| !es_cromo(n));
        if id == target || !movible(id) || !movible(target) {
            return self.clone();
        }
        let Some(nodo) = self.find_slot(id).cloned() else {
            return self.clone();
        };
        // El CENTRO solo junta lo que ya es de la misma familia: un listado
        // con listados, un panel con paneles (ADR 0134). Un listado metido
        // en las pestañas de los sitios viviría en dieciséis columnas, y un
        // grupo mezclado dejaría de ser un grupo de paneles para siempre.
        if zona == DropZone::Center
            && self
                .find_slot(target)
                .is_none_or(|t| es_panel(t) != es_panel(&nodo))
        {
            return self.clone();
        }
        let Some(resto) = self.close_slot(id) else {
            return self.clone();
        };
        match zona.edge() {
            None => resto.add_tab(target, &nodo),
            Some(edge) => resto
                .place_beside(target, edge, &nodo)
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
    fn es_unidad_de(&self, target: SlotId) -> bool {
        match self {
            Self::Slot { id, .. } => *id == target,
            Self::Tabs { children, .. } => children
                .iter()
                .any(|c| matches!(c, Self::Slot { id, .. } if *id == target)),
            Self::Split { .. } => false,
        }
    }

    /// `nodo` beside `edge` of `target`'s unit; `None` if it is not there.
    fn place_beside(&self, target: SlotId, edge: Edge, nodo: &Self) -> Option<Self> {
        if self.es_unidad_de(target) {
            let (children, sizes) = if edge.is_front() {
                (vec![nodo.clone(), self.clone()], vec![Size::Weight(1); 2])
            } else {
                (vec![self.clone(), nodo.clone()], vec![Size::Weight(1); 2])
            };
            return Some(Self::Split {
                dir: edge.axis(),
                children,
                sizes,
            });
        }
        let hijos = match self {
            Self::Split { children, .. } | Self::Tabs { children, .. } => children,
            Self::Slot { .. } => return None,
        };
        let pos = hijos.iter().position(|c| c.contains(target))?;
        // Hermano en el MISMO reparto, si corre en el eje y la unidad pesa:
        // así se parte a partes iguales, como `split_slot`. Nunca en el
        // reparto del cromo: ese no se gira, y lo que se soltara ahí ya no
        // se podría girar de vuelta con `layout.flip`.
        if let Self::Split {
            dir,
            children,
            sizes,
        } = self
            && *dir == edge.axis()
            && !children.iter().any(es_cromo)
            && children[pos].es_unidad_de(target)
        {
            // Junto a un panel de ancho FIJO tambien entra como hermano, con
            // peso: partirlo por dentro le daría la mitad de sus dieciséis
            // columnas a un listado.
            let tam = match sizes.get(pos).copied().unwrap_or(Size::Weight(1)) {
                Size::Weight(peso) => Size::Weight(peso),
                Size::Fixed(_) | Size::Auto => peso_entre_hermanos(Size::Weight(1), sizes),
            };
            let mut nc = children.clone();
            let mut ns = sizes.clone();
            ns.resize(nc.len(), Size::Weight(1));
            let at = if edge.is_front() { pos } else { pos + 1 };
            nc.insert(at, nodo.clone());
            ns.insert(at, tam);
            return Some(Self::Split {
                dir: *dir,
                children: nc,
                sizes: ns,
            });
        }
        let dentro = hijos[pos].place_beside(target, edge, nodo)?;
        Some(self.with_child(pos, dentro))
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
            Giro::Hecho(n) => n,
            Giro::Rehusado | Giro::NoEsta => self.clone(),
        }
    }

    fn flip_inner(&self, id: SlotId) -> Giro {
        let hijos = match self {
            Self::Split { children, .. } | Self::Tabs { children, .. } => children,
            Self::Slot { .. } => return Giro::NoEsta,
        };
        let Some(pos) = hijos.iter().position(|c| c.contains(id)) else {
            return Giro::NoEsta;
        };
        match hijos[pos].flip_inner(id) {
            Giro::Hecho(dentro) => return Giro::Hecho(self.with_child(pos, dentro)),
            Giro::Rehusado => return Giro::Rehusado,
            Giro::NoEsta => {}
        }
        let Self::Split {
            dir,
            children,
            sizes,
        } = self
        else {
            return Giro::NoEsta;
        };
        if children.iter().any(es_cromo) {
            return Giro::Rehusado;
        }
        let otro = match dir {
            Dir::Horizontal => Dir::Vertical,
            Dir::Vertical => Dir::Horizontal,
        };
        let pesa = |i: usize| matches!(sizes.get(i), Some(Size::Weight(_)) | None);
        if !pesa(pos) {
            return Giro::Rehusado;
        }
        let mut desde = pos;
        while desde > 0 && pesa(desde - 1) {
            desde -= 1;
        }
        let mut hasta = pos + 1;
        while hasta < children.len() && pesa(hasta) {
            hasta += 1;
        }
        if hasta - desde < 2 {
            return Giro::Rehusado;
        }
        if desde == 0 && hasta == children.len() {
            return Giro::Hecho(Self::Split {
                dir: otro,
                children: children.clone(),
                sizes: sizes.clone(),
            });
        }
        let cuanto = |i: usize| match sizes.get(i) {
            Some(Size::Weight(w)) => u32::from(*w),
            _ => 1,
        };
        let total = u16::try_from((desde..hasta).map(cuanto).sum::<u32>()).unwrap_or(u16::MAX);
        let racha = Self::Split {
            dir: otro,
            children: children[desde..hasta].to_vec(),
            sizes: (desde..hasta)
                .map(|i| sizes.get(i).copied().unwrap_or(Size::Weight(1)))
                .collect(),
        };
        let mut nc = children[..desde].to_vec();
        nc.push(racha);
        nc.extend_from_slice(&children[hasta..]);
        let mut ns: Vec<Size> = (0..desde)
            .map(|i| sizes.get(i).copied().unwrap_or(Size::Weight(1)))
            .collect();
        ns.push(Size::Weight(total.max(1)));
        ns.extend(
            (hasta..children.len()).map(|i| sizes.get(i).copied().unwrap_or(Size::Weight(1))),
        );
        Giro::Hecho(Self::Split {
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
        /// Celdas por pulsación en un hijo fijo.
        const PASO: i32 = 2;
        self.map_split_of(id, &|sizes, pos| {
            let mut ns = sizes.to_vec();
            match ns.get(pos) {
                Some(Size::Weight(w)) => {
                    let nuevo = i32::from(*w).saturating_add(i32::from(delta)).clamp(1, 10);
                    ns[pos] = Size::Weight(u16::try_from(nuevo).unwrap_or(1));
                }
                Some(Size::Fixed(n)) => {
                    let nuevo = i32::from(*n)
                        .saturating_add(i32::from(delta).saturating_mul(PASO))
                        .clamp(2, 100);
                    ns[pos] = Size::Fixed(u16::try_from(nuevo).unwrap_or(2));
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
    /// With weights the pair is renormalized to a hundred (`PESO_FINO`) so
    /// the drag has granularity: two slots by default are `Weight(1)` and
    /// `Weight(1)`, and over that pair only the exact half would exist.
    ///
    /// `frac` is clamped so neither of the two disappears: a slot at zero
    /// takes with it the way to bring it back.
    ///
    /// [`Size::Auto`] is not touched, for the same reason as in
    /// [`Self::resize`]: layout replaces it and a number saved here would
    /// be overwritten by the next frame.
    /// `celdas_del_par` is what the two occupy together, in layout cells.
    /// Whoever paints knows it, not the tree: a [`Size::Fixed`] is measured
    /// in cells and a fraction alone is not enough to write it.
    #[must_use]
    pub fn drag_border(&self, id: SlotId, frac: f32, celdas_del_par: u16) -> Self {
        self.map_split_of(id, &|sizes, pos| {
            arrastrar_pareja(sizes, pos, frac, celdas_del_par)
        })
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
        let hijos = match self {
            Self::Split { children, .. } | Self::Tabs { children, .. } => children,
            Self::Slot { .. } => return None,
        };
        let pos = hijos.iter().position(|c| c.contains(a))?;
        if hijos[pos].contains(b) {
            return hijos[pos].border_pair(a, b);
        }
        if matches!(self, Self::Split { .. })
            && let Some(siguiente) = hijos.get(pos + 1)
            && siguiente.contains(b)
        {
            return Some((hijos[pos].slot_ids(), siguiente.slot_ids()));
        }
        None
    }

    /// Like [`Self::drag_border`], but on the border between `a` and `b` in
    /// the layout where they are neighbors ([`Self::border_pair`]), with
    /// `frac` and `celdas_del_par` measured over the whole TWO children.
    /// This way the border that was grabbed moves, even if `a` is the last
    /// one of its own layout.
    #[must_use]
    pub fn drag_border_between(
        &self,
        a: SlotId,
        b: SlotId,
        frac: f32,
        celdas_del_par: u16,
    ) -> Self {
        let hijos = match self {
            Self::Split { children, .. } | Self::Tabs { children, .. } => children,
            Self::Slot { .. } => return self.clone(),
        };
        let Some(pos) = hijos.iter().position(|c| c.contains(a)) else {
            return self.clone();
        };
        if hijos[pos].contains(b) {
            let dentro = hijos[pos].drag_border_between(a, b, frac, celdas_del_par);
            return self.with_child(pos, dentro);
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
                sizes: arrastrar_pareja(sizes, pos, frac, celdas_del_par),
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
                    otro => *otro,
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
        let hijos = match self {
            Self::Split { children, .. } | Self::Tabs { children, .. } => children,
            Self::Slot { .. } => return None,
        };
        for c in hijos {
            if c.contains(id)
                && !matches!(c, Self::Slot { .. })
                && let Some(dentro) = c.sizes_of(id)
            {
                return Some(dentro);
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
        let hijos = match self {
            Self::Split { children, .. } | Self::Tabs { children, .. } => children,
            Self::Slot { .. } => return self.clone(),
        };
        for (i, c) in hijos.iter().enumerate() {
            if c.contains(id) && !matches!(c, Self::Slot { .. }) {
                let dentro = c.map_split_of(id, f);
                if dentro != *c {
                    return self.with_child(i, dentro);
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

    /// Opens `nuevo` as a tab next to `id`, and leaves it active.
    ///
    /// If `id` was not in tabs, it wraps it first: opening a tab from a
    /// lone panel is what turns that panel into the first of a group, and
    /// asking the user for two steps for that would make no sense.
    #[must_use]
    pub fn add_tab(&self, id: SlotId, nuevo: &Self) -> Self {
        let envuelto = self.wrap_in_tabs(id);
        envuelto.insert_tab_near(id, nuevo).unwrap_or(envuelto)
    }

    fn insert_tab_near(&self, id: SlotId, nuevo: &Self) -> Option<Self> {
        let hijos = match self {
            Self::Split { children, .. } | Self::Tabs { children, .. } => children,
            Self::Slot { .. } => return None,
        };
        // Deeper inside first: the `Tabs` that rules is the INNER one, not
        // the one wrapping half the screen.
        for (i, c) in hijos.iter().enumerate() {
            if let Some(cambiado) = c.insert_tab_near(id, nuevo) {
                return Some(self.with_child(i, cambiado));
            }
        }
        if let Self::Tabs { children, .. } = self
            && let Some(pos) = children.iter().position(|c| c.contains(id))
        {
            let mut nuevos = children.clone();
            nuevos.insert(pos + 1, nuevo.clone());
            return Some(Self::Tabs {
                children: nuevos,
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
        let hijos = match self {
            Self::Split { children, .. } | Self::Tabs { children, .. } => children,
            Self::Slot { .. } => return None,
        };
        for (i, c) in hijos.iter().enumerate() {
            if let Some(cambiado) = c.close_tab(id) {
                return Some(self.with_child(i, cambiado));
            }
        }
        if let Self::Tabs { children, active } = self
            && children.len() > 1
            && let Some(pos) = children.iter().position(|c| c.contains(id))
        {
            let mut nuevos = children.clone();
            nuevos.remove(pos);
            if nuevos.len() == 1 {
                return nuevos.into_iter().next();
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
            .min(nuevos.len() - 1);
            return Some(Self::Tabs {
                children: nuevos,
                active: act,
            });
        }
        None
    }

    /// Las pestañas del grupo que contiene `id`: el hueco que encabeza cada
    /// una y cuál está activa. `None` si `id` no está en un grupo.
    ///
    /// Lo usa el render de la barra de pestañas, y por eso devuelve el PRIMER
    /// hueco de cada pestaña: es de quien se saca el título.
    #[must_use]
    pub fn tabs_of(&self, id: SlotId) -> Option<(Vec<SlotId>, usize)> {
        let hijos = match self {
            Self::Split { children, .. } | Self::Tabs { children, .. } => children,
            Self::Slot { .. } => return None,
        };
        for c in hijos {
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

    /// El mismo árbol con el hueco `id` VISIBLE: activa su pestaña en cada
    /// grupo del camino (#329).
    ///
    /// Existe porque [`Self::slot_ids`] y [`Self::visible_slot_ids`] contestan
    /// dos preguntas distintas —«¿existe?» y «¿se ve?»— y hay una tercera que
    /// no tenía respuesta: «que se vea». Sin ella, quien encontraba un hueco
    /// escondido solo podía mandarle el teclado, que es enfocar algo que el
    /// lector no tiene delante.
    ///
    /// Recorre TODO el camino y no solo el grupo de dentro: activar la pestaña
    /// interior dejando la exterior en otra deja el hueco igual de invisible, y
    /// el llamante creería haberlo enseñado. Un hueco que no está devuelve el
    /// árbol igual — esto asegura un invariante, no ejecuta un gesto.
    ///
    /// Uno que ya se ve lo devuelve igual salvo en un caso, y conviene decirlo:
    /// el tipo permite un `active` fuera de rango, que [`Self::visible_slot_ids`]
    /// y el reparto clampan los dos al primero. Sobre uno así, revelar el hueco
    /// que YA se veía escribe el índice de verdad. Normaliza, no mueve nada de
    /// sitio.
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
            // El `active` de entrada no se lee a propósito: revelar no lo
            // conserva ni lo mueve un paso, lo FIJA en la pestaña que contiene
            // al hueco. Ese es todo el gesto.
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

    /// Deja activa la pestaña `i` del grupo que contiene `id`.
    #[must_use]
    pub fn set_active_for(&self, id: SlotId, i: usize) -> Self {
        self.map_tabs_of(id, &|children, _| {
            (children.to_vec(), i.min(children.len() - 1))
        })
    }

    /// Mueve la pestaña que contiene `id` `delta` posiciones, sin salirse.
    #[must_use]
    pub fn move_tab(&self, id: SlotId, delta: isize) -> Self {
        self.map_tabs_of(id, &|children, pos| {
            let destino = pos
                .saturating_add_signed(delta)
                .min(children.len().saturating_sub(1));
            let mut nuevos = children.to_vec();
            let quien = nuevos.remove(pos);
            nuevos.insert(destino, quien);
            (nuevos, destino)
        })
    }

    /// Aplica `f` a la `Tabs` que contiene `id`, dándole sus hijos y la
    /// posición del que lo contiene, y esperando los hijos nuevos y el activo.
    fn map_tabs_of(&self, id: SlotId, f: &ReTab<'_>) -> Self {
        let hijos = match self {
            Self::Split { children, .. } | Self::Tabs { children, .. } => children,
            Self::Slot { .. } => return self.clone(),
        };
        for (i, c) in hijos.iter().enumerate() {
            if c.contains(id) && !matches!(c, Self::Slot { .. }) {
                let dentro = c.map_tabs_of(id, f);
                if dentro != *c {
                    return self.with_child(i, dentro);
                }
            }
        }
        if let Self::Tabs { children, .. } = self
            && let Some(pos) = children.iter().position(|c| c.contains(id))
        {
            let (nuevos, act) = f(children, pos);
            return Self::Tabs {
                children: nuevos,
                active: act,
            };
        }
        self.clone()
    }

    /// El mismo nodo con el hijo `i` sustituido.
    fn with_child(&self, i: usize, hijo: Self) -> Self {
        match self {
            Self::Split {
                dir,
                children,
                sizes,
            } => {
                let mut nuevos = children.clone();
                if let Some(slot) = nuevos.get_mut(i) {
                    *slot = hijo;
                }
                Self::Split {
                    dir: *dir,
                    children: nuevos,
                    sizes: sizes.clone(),
                }
            }
            Self::Tabs { children, active } => {
                let mut nuevos = children.clone();
                if let Some(slot) = nuevos.get_mut(i) {
                    *slot = hijo;
                }
                Self::Tabs {
                    children: nuevos,
                    active: *active,
                }
            }
            Self::Slot { .. } => self.clone(),
        }
    }

    /// Una copia cuyos huecos se numeran desde `base`, más el mapa
    /// viejo → nuevo.
    ///
    /// Es lo que hace que dos perfiles no se pisen (spec 2026-08-26, D5): las
    /// disposiciones de fábrica usan 1..=8 TODAS, así que adoptar la misma en
    /// dos perfiles sin reasignar deja los dos compartiendo el hueco 1 —
    /// mismo directorio, mismo historial, mismas marcas.
    ///
    /// El mapa NO es una comodidad. `[profile.start]` viene indexado por los
    /// ids que el fichero de disposición del perfil escribe, así que aplicarlo
    /// después de rebasar exige la traducción; devolver solo el árbol dejaría
    /// esas claves inservibles.
    ///
    /// El orden de asignación es el de [`Self::slot_ids`], que es el de
    /// lectura: determinista, y por tanto el mismo árbol rebasado dos veces
    /// desde la misma base da el mismo resultado.
    ///
    /// # De dónde sale `base`
    ///
    /// De [`crate::session::SessionBody::next_slot_base`], y de ningún otro
    /// sitio. La propiedad de «no colisiona» vive ENTERA ahí: mirar solo el
    /// árbol del perfil activo daría una base que aterriza encima de los
    /// huecos huérfanos, que son justo los que nadie está mirando cuando pasa.
    /// Y el árbol rebasado se mete en `layouts` ANTES de volver a pedir una
    /// base, o dos perfiles rebasan desde el mismo número.
    ///
    /// Sin espacio libre por arriba devuelve el árbol SIN TOCAR y un mapa
    /// vacío: el llamante se queda como estaba en vez de recibir un árbol con
    /// ids repetidos.
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
    /// let (nuevo, mapa) = arbol.rebase_slot_ids(100);
    /// assert_eq!(nuevo.slot_ids(), vec![SlotId(100), SlotId(101)]);
    /// assert_eq!(mapa[&SlotId(2)], SlotId(101));
    /// ```
    #[must_use]
    pub fn rebase_slot_ids(&self, base: u32) -> (Self, std::collections::BTreeMap<SlotId, SlotId>) {
        let mut mapa = std::collections::BTreeMap::new();
        let mut siguiente = base;
        for id in self.slot_ids() {
            // Un árbol con ids repetidos es incoherente de entrada
            // (`duplicate_slot_ids` lo dice y `validate` lo rechaza); si llega
            // uno, los dos huecos siguen compartiendo id en vez de que uno se
            // lleve un número que nadie le dio.
            if mapa.contains_key(&id) {
                continue;
            }
            // Sin espacio arriba se DEVUELVE EL ÁRBOL TAL CUAL, y esto no es
            // celo: saturar era peor que envolver. `saturating_add` deja a
            // todos los huecos siguientes con `u32::MAX`, así que un árbol de
            // entrada sano salía con ids REPETIDOS; ese árbol se guarda en
            // `layouts`, y el siguiente `from_value` lo valida y devuelve
            // `BadLayout` para el cuerpo ENTERO — el estado de todos los
            // perfiles, no el del roto. Ésa es la pérdida que ADR 0059 promete
            // que no pasa.
            let Some(tope) = siguiente.checked_add(1) else {
                return (self.clone(), std::collections::BTreeMap::new());
            };
            mapa.insert(id, SlotId(siguiente));
            siguiente = tope;
        }
        (self.remap_slot_ids(&mapa), mapa)
    }

    /// Aplica un mapa de ids a una copia del árbol. Lo que no esté en el mapa
    /// se queda como está.
    fn remap_slot_ids(&self, mapa: &std::collections::BTreeMap<SlotId, SlotId>) -> Self {
        match self {
            Self::Split {
                dir,
                children,
                sizes,
            } => Self::Split {
                dir: *dir,
                children: children.iter().map(|c| c.remap_slot_ids(mapa)).collect(),
                sizes: sizes.clone(),
            },
            Self::Tabs { children, active } => Self::Tabs {
                children: children.iter().map(|c| c.remap_slot_ids(mapa)).collect(),
                active: *active,
            },
            Self::Slot {
                id,
                kind,
                params,
                bindings,
            } => Self::Slot {
                id: mapa.get(id).copied().unwrap_or(*id),
                kind: kind.clone(),
                params: params.clone(),
                bindings: *bindings,
            },
        }
    }

    /// Los ids repetidos, si los hay. Un layout con dos huecos del mismo id es
    /// incoherente y NO se adivina cuál gana.
    #[must_use]
    pub fn duplicate_slot_ids(&self) -> Vec<SlotId> {
        let mut cuenta: BTreeMap<SlotId, usize> = BTreeMap::new();
        for id in self.slot_ids() {
            *cuenta.entry(id).or_default() += 1;
        }
        cuenta
            .into_iter()
            .filter_map(|(id, n)| (n > 1).then_some(id))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **Sacar a la luz un hueco escondido activa SU pestaña** (#329).
    ///
    /// La barra de paneles y los toggles preguntan «¿existe?» y actúan como si
    /// hubieran preguntado «¿se ve?». Esto es la mitad que faltaba: poder
    /// contestar «que se vea».
    #[test]
    fn revelar_activa_la_pestana_del_hueco() {
        let arbol = Node::Tabs {
            children: vec![
                Node::slot(SlotId(1), KindId::browser()),
                Node::slot(SlotId(2), KindId::new("log")),
            ],
            active: 0,
        };
        assert!(!arbol.visible_slot_ids().contains(&SlotId(2)));
        let visto = arbol.reveal(SlotId(2));
        assert!(visto.visible_slot_ids().contains(&SlotId(2)));
        assert!(
            !visto.visible_slot_ids().contains(&SlotId(1)),
            "activar una pestaña esconde a su hermana: es lo que significa"
        );
    }

    /// Y lo hace en CADA grupo del camino, no solo en el de dentro.
    ///
    /// Con grupos anidados, activar el interior y dejar el exterior en otra
    /// pestaña deja el hueco tan invisible como estaba, y el llamante creería
    /// haberlo enseñado.
    #[test]
    fn revelar_atraviesa_los_grupos_anidados() {
        let dentro = Node::Tabs {
            children: vec![
                Node::slot(SlotId(3), KindId::browser()),
                Node::slot(SlotId(4), KindId::new("log")),
            ],
            active: 0,
        };
        let arbol = Node::Tabs {
            children: vec![Node::slot(SlotId(5), KindId::browser()), dentro],
            active: 0,
        };
        assert!(!arbol.visible_slot_ids().contains(&SlotId(4)));
        let visto = arbol.reveal(SlotId(4));
        assert!(
            visto.visible_slot_ids().contains(&SlotId(4)),
            "el grupo de fuera seguía enseñando la otra pestaña"
        );
    }

    /// Con un `Split` por el camino, revelar respeta TODO lo demás: los
    /// tamaños, y la pestaña activa de un grupo que no contiene al hueco.
    ///
    /// El riesgo de una función que reconstruye el árbol es perder por el
    /// camino algo que nadie mira en el test, y aquí lo que se perdería son
    /// medidas: un `Split` que vuelve con pesos por defecto reparte la pantalla
    /// de otra manera sin que nada se ponga rojo.
    #[test]
    fn revelar_conserva_medidas_y_los_grupos_ajenos() {
        let ajeno = Node::Tabs {
            children: vec![
                Node::slot(SlotId(10), KindId::browser()),
                Node::slot(SlotId(11), KindId::browser()),
            ],
            active: 1,
        };
        let arbol = Node::Split {
            dir: Dir::Horizontal,
            sizes: vec![Size::Fixed(24), Size::Weight(1)],
            children: vec![
                ajeno,
                Node::Tabs {
                    children: vec![
                        Node::slot(SlotId(1), KindId::browser()),
                        Node::slot(SlotId(2), KindId::new("log")),
                    ],
                    active: 0,
                },
            ],
        };
        let visto = arbol.reveal(SlotId(2));
        assert!(visto.visible_slot_ids().contains(&SlotId(2)));
        assert!(
            visto.visible_slot_ids().contains(&SlotId(11)),
            "el grupo de al lado no se toca: no contiene al hueco"
        );
        let (sizes, _) = visto.sizes_of(SlotId(2)).expect("sigue en el split");
        assert_eq!(
            (sizes[0], sizes[1]),
            (Size::Fixed(24), Size::Weight(1)),
            "reconstruir el split se llevó por delante las medidas"
        );
    }

    /// Un hueco que ya se ve —o que no está— no mueve nada: revelar no es un
    /// gesto, es un invariante que se asegura.
    #[test]
    fn revelar_lo_que_ya_se_ve_no_cambia_el_arbol() {
        let arbol = Node::Tabs {
            children: vec![
                Node::slot(SlotId(1), KindId::browser()),
                Node::slot(SlotId(2), KindId::new("log")),
            ],
            active: 1,
        };
        assert_eq!(arbol.reveal(SlotId(2)), arbol);
        assert_eq!(arbol.reveal(SlotId(99)), arbol, "y uno que no está tampoco");
    }

    /// Arrastrar el borde pone el hueco donde dice el puntero, y lo que uno
    /// gana lo pierde su vecino.
    ///
    /// Con pesos se renormaliza la pareja: dos huecos por defecto son
    /// `Weight(1)` y `Weight(1)`, y sobre esa pareja el único borde posible
    /// sería la mitad exacta — un arrastre que solo puede aterrizar en el
    /// centro no es un arrastre.
    #[test]
    fn arrastrar_el_borde_reparte_la_pareja() {
        let arbol = Node::split(
            Dir::Horizontal,
            vec![
                Node::slot(SlotId(1), KindId::browser()),
                Node::slot(SlotId(2), KindId::browser()),
            ],
        );
        let movido = arbol.drag_border(SlotId(1), 0.25, 80);
        let (sizes, pos) = movido.sizes_of(SlotId(1)).expect("está en un split");
        assert_eq!(pos, 0);
        assert_eq!(
            (sizes[0], sizes[1]),
            (Size::Weight(25), Size::Weight(75)),
            "un cuarto para el de la izquierda, y el resto para el otro"
        );
    }

    /// Ni el uno ni el otro pueden desaparecer: un hueco a cero se lleva con
    /// él la forma de devolverlo.
    #[test]
    fn arrastrar_hasta_el_extremo_deja_hueco_a_los_dos() {
        let arbol = Node::split(
            Dir::Horizontal,
            vec![
                Node::slot(SlotId(1), KindId::browser()),
                Node::slot(SlotId(2), KindId::browser()),
            ],
        );
        for frac in [-3.0, 0.0, 1.0, 4.0] {
            let movido = arbol.drag_border(SlotId(1), frac, 80);
            let (sizes, _) = movido.sizes_of(SlotId(1)).expect("split");
            for s in &sizes[..2] {
                assert!(
                    matches!(s, Size::Weight(w) if *w >= 1),
                    "con frac={frac} alguien se quedó sin sitio: {sizes:?}"
                );
            }
        }
    }

    /// Un FIJO se escribe en celdas —es lo que significa— y su vecino
    /// ponderado no se convierte en fijo: si lo hiciera, dejaría de estirarse
    /// al cambiar el tamaño de la ventana.
    #[test]
    fn arrastrar_el_borde_de_un_fijo_lo_escribe_en_celdas() {
        let arbol = Node::Split {
            dir: Dir::Horizontal,
            children: vec![
                Node::slot(SlotId(1), KindId::new("places")),
                Node::slot(SlotId(2), KindId::browser()),
            ],
            sizes: vec![Size::Fixed(16), Size::Weight(1)],
        };
        let movido = arbol.drag_border(SlotId(1), 0.5, 100);
        let (sizes, _) = movido.sizes_of(SlotId(1)).expect("split");
        assert_eq!(sizes[0], Size::Fixed(50), "la mitad de cien celdas");
        assert_eq!(sizes[1], Size::Weight(1), "el ponderado sigue ponderado");
    }

    /// Las disposiciones de fábrica usan 1..=8, TODAS. Sin rebase, dos perfiles
    /// comparten el hueco 1 y se pisan el directorio y el historial — que es
    /// exactamente el bug que los perfiles existen para arreglar.
    #[test]
    fn rebase_reasigna_desde_la_base_y_devuelve_el_mapa() {
        let arbol = Node::split(
            Dir::Horizontal,
            vec![
                Node::slot(SlotId(1), KindId::browser()),
                Node::slot(SlotId(2), KindId::browser()),
            ],
        );
        let (nuevo, mapa) = arbol.rebase_slot_ids(100);
        assert_eq!(nuevo.slot_ids(), vec![SlotId(100), SlotId(101)]);
        assert_eq!(mapa.get(&SlotId(1)), Some(&SlotId(100)));
        assert_eq!(mapa.get(&SlotId(2)), Some(&SlotId(101)));
    }

    /// Rebasar no puede cambiar la FORMA: mismo árbol, mismos kinds, mismos
    /// tamaños. Solo los números.
    #[test]
    fn rebase_conserva_la_forma() {
        let arbol = Node::split(
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
        let (nuevo, _) = arbol.rebase_slot_ids(50);
        assert_eq!(nuevo.slot_ids().len(), arbol.slot_ids().len());
        assert!(crate::layout::validate(&nuevo).is_ok());
    }

    /// Un árbol sano rebasado no puede FABRICAR un duplicado, sea cual sea el
    /// orden de los ids originales.
    #[test]
    fn rebase_no_fabrica_duplicados() {
        let arbol = Node::split(
            Dir::Horizontal,
            vec![
                Node::slot(SlotId(7), KindId::browser()),
                Node::slot(SlotId(1), KindId::browser()),
            ],
        );
        let (nuevo, _) = arbol.rebase_slot_ids(10);
        assert!(nuevo.duplicate_slot_ids().is_empty());
    }

    /// El árbol hace round-trip: es el MISMO formato que el fichero de config,
    /// el blob de sesión de L2 y lo que escupirá el editor de layouts. Un
    /// formato que no round-trippea obliga a migrar entre dos, que es justo lo
    /// que la ADR 0058 evita.
    #[test]
    fn el_arbol_hace_round_trip() {
        let arbol = Node::Split {
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
        let json = serde_json::to_string(&arbol).expect("serializa");
        assert_eq!(serde_json::from_str::<Node>(&json).expect("vuelve"), arbol);
    }

    /// Un kind DESCONOCIDO sobrevive al round-trip con sus `params` intactos.
    /// Es la regla 3 del modelo: un cliente que no sabe pintar un kind no puede
    /// borrárselo del layout al otro.
    #[test]
    fn un_kind_desconocido_conserva_sus_params() {
        let json = r#"{"slot":{"id":7,"kind":"terminal","params":{"shell":"fish"},"bindings":{}}}"#;
        let n: Node = serde_json::from_str(json).expect("un kind que no conocemos parsea");
        let vuelta = serde_json::to_string(&n).expect("serializa");
        assert!(
            vuelta.contains("\"shell\":\"fish\""),
            "los params se pierden: {vuelta}"
        );
    }

    /// Los ids visibles de un árbol, en orden de lectura. Lo usan roles, store
    /// y resolve, así que se prueba aquí una vez.
    #[test]
    fn slot_ids_recorre_tambien_las_pestanas_ocultas() {
        let arbol = Node::Tabs {
            active: 0,
            children: vec![
                Node::slot(SlotId(1), KindId::browser()),
                Node::slot(SlotId(2), KindId::browser()),
            ],
        };
        assert_eq!(arbol.slot_ids(), vec![SlotId(1), SlotId(2)]);
    }

    /// `substitute_auto` cambia los `Auto` por `Fixed` y NO toca nada más: el
    /// árbol guardado conserva sus `Auto`, el del frame no los tiene.
    #[test]
    fn substitute_auto_solo_cambia_los_auto() {
        let arbol = Node::Split {
            dir: Dir::Vertical,
            sizes: vec![Size::Weight(1), Size::Auto, Size::Fixed(1)],
            children: vec![
                Node::slot(SlotId(1), KindId::browser()),
                Node::slot(SlotId(2), KindId::new("tasks")),
                Node::slot(SlotId(3), KindId::new("status")),
            ],
        };
        let del_frame = arbol.substitute_auto(&|id| if id == SlotId(2) { (0, 4) } else { (0, 0) });
        let Node::Split { sizes, .. } = &del_frame else {
            panic!("sigue siendo un split")
        };
        assert_eq!(
            *sizes,
            vec![Size::Weight(1), Size::Fixed(4), Size::Fixed(1)]
        );
        let Node::Split { sizes: orig, .. } = &arbol else {
            panic!("split")
        };
        assert_eq!(orig[1], Size::Auto, "el árbol guardado no se toca");
    }

    /// En un corte HORIZONTAL, `Auto` toma el ANCHO natural, no el alto. Una
    /// sidebar mide lo que mide de ancha; su alto lo pone el reparto.
    #[test]
    fn substitute_auto_toma_el_eje_del_corte() {
        let arbol = Node::Split {
            dir: Dir::Horizontal,
            sizes: vec![Size::Auto, Size::Weight(1)],
            children: vec![
                Node::slot(SlotId(1), KindId::new("places")),
                Node::slot(SlotId(2), KindId::browser()),
            ],
        };
        let del_frame = arbol.substitute_auto(&|_| (18, 3));
        let Node::Split { sizes, .. } = &del_frame else {
            panic!("split")
        };
        assert_eq!(sizes[0], Size::Fixed(18), "el ancho, no el alto");
    }

    /// El `Auto` de un subárbol se apoya en su PRIMER hueco: es el único que
    /// no depende de cómo se reparta después.
    #[test]
    fn el_primer_hueco_es_a_quien_se_le_pregunta() {
        let arbol = Node::split(
            Dir::Horizontal,
            vec![
                Node::slot(SlotId(7), KindId::browser()),
                Node::slot(SlotId(8), KindId::browser()),
            ],
        );
        assert_eq!(arbol.first_slot_id(), Some(SlotId(7)));
    }

    fn b(id: u32) -> Node {
        Node::slot(SlotId(id), KindId::browser())
    }

    /// Abrir una pestaña desde un panel suelto lo envuelve y deja activa la
    /// nueva. Pedir dos pasos para eso no tendría sentido.
    #[test]
    fn abrir_una_pestana_desde_un_panel_suelto_lo_envuelve() {
        let arbol = Node::split(Dir::Horizontal, vec![b(1), b(2)]);
        let nuevo = arbol.add_tab(SlotId(2), &b(9));
        assert_eq!(nuevo.slot_ids(), vec![SlotId(1), SlotId(2), SlotId(9)]);
        assert_eq!(
            nuevo.tabs_of(SlotId(2)),
            Some((vec![SlotId(2), SlotId(9)], 1))
        );
    }

    /// La `Tabs` que manda es la INTERIOR, no la que envuelve media pantalla.
    #[test]
    fn una_pestana_nueva_entra_en_el_grupo_mas_interior() {
        let arbol = Node::Tabs {
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
        let nuevo = arbol.add_tab(SlotId(2), &b(9));
        assert_eq!(
            nuevo.tabs_of(SlotId(2)),
            Some((vec![SlotId(2), SlotId(9)], 1))
        );
        // El grupo de fuera sigue con una sola pestaña.
        assert_eq!(nuevo.tabs_of(SlotId(1)), Some((vec![SlotId(1)], 0)));
    }

    /// Una `Tabs` que se queda con UN hijo se DISUELVE: un grupo de una
    /// pestaña no es un grupo, y dejarlo pintaría una barra con una sola
    /// entrada para siempre.
    #[test]
    fn cerrar_la_penultima_pestana_disuelve_el_grupo() {
        let arbol = Node::Tabs {
            children: vec![b(1), b(2)],
            active: 1,
        };
        let nuevo = arbol.close_tab(SlotId(2)).expect("estaba en un grupo");
        assert_eq!(nuevo, b(1), "el grupo desaparece y queda el hueco");
    }

    /// Cerrar un panel SUELTO no es `pane.tab-close`: devuelve `None` y el llamante
    /// decide (será `layout.close-slot`).
    #[test]
    fn cerrar_un_panel_suelto_no_es_cerrar_una_pestana() {
        let arbol = Node::split(Dir::Horizontal, vec![b(1), b(2)]);
        assert_eq!(arbol.close_tab(SlotId(2)), None);
    }

    /// Cerrar una pestaña anterior a la activa arrastra el índice activo: si
    /// no, el activo pasaría a nombrar a la pestaña de al lado.
    #[test]
    fn cerrar_una_pestana_anterior_arrastra_el_activo() {
        let arbol = Node::Tabs {
            children: vec![b(1), b(2), b(3)],
            active: 2,
        };
        let nuevo = arbol.close_tab(SlotId(1)).expect("está en el grupo");
        assert_eq!(
            nuevo.tabs_of(SlotId(3)),
            Some((vec![SlotId(2), SlotId(3)], 1))
        );
    }

    /// Mover una pestaña se la lleva el activo con ella.
    #[test]
    fn mover_una_pestana_se_lleva_el_activo() {
        let arbol = Node::Tabs {
            children: vec![b(1), b(2), b(3)],
            active: 0,
        };
        let nuevo = arbol.move_tab(SlotId(1), 2);
        assert_eq!(
            nuevo.tabs_of(SlotId(1)),
            Some((vec![SlotId(2), SlotId(3), SlotId(1)], 2))
        );
    }

    /// Mover más allá del borde se queda en el borde, no da la vuelta: una
    /// pestaña que salta de la última a la primera al pulsar una vez de más es
    /// exactamente lo que nadie quería.
    #[test]
    fn mover_una_pestana_no_da_la_vuelta() {
        let arbol = Node::Tabs {
            children: vec![b(1), b(2)],
            active: 1,
        };
        let nuevo = arbol.move_tab(SlotId(2), 5);
        assert_eq!(
            nuevo.tabs_of(SlotId(2)),
            Some((vec![SlotId(1), SlotId(2)], 1))
        );
    }

    /// Partir un hueco lo deja con el nuevo al lado, los dos al mismo peso.
    #[test]
    fn partir_un_hueco_deja_a_los_dos_al_mismo_peso() {
        let arbol = b(1);
        let nuevo = arbol.split_slot(SlotId(1), Dir::Vertical, &b(9));
        assert_eq!(nuevo.slot_ids(), vec![SlotId(1), SlotId(9)]);
        let Node::Split { dir, sizes, .. } = &nuevo else {
            panic!("split")
        };
        assert_eq!(*dir, Dir::Vertical);
        assert_eq!(*sizes, vec![Size::Weight(1), Size::Weight(1)]);
    }

    /// Partir OTRA VEZ en el mismo eje da TERCIOS, no un cuarto.
    ///
    /// El corte se une al `Split` que ya corre en ese eje en vez de envolver
    /// el hueco en uno nuevo. Anidando, cada partición se llevaba la mitad de
    /// la mitad: tres paneles quedaban en 1/2, 1/4 y 1/4, y a la cuarta el
    /// hijo más profundo bajaba del mínimo del kind y el reparto lo degradaba
    /// a pestañas — el panel recién pedido desaparecía sin decir nada.
    #[test]
    fn partir_en_el_mismo_eje_reparte_a_partes_iguales() {
        let arbol = b(1).split_slot(SlotId(1), Dir::Vertical, &b(2));
        let tres = arbol.split_slot(SlotId(2), Dir::Vertical, &b(3));
        let Node::Split {
            children, sizes, ..
        } = &tres
        else {
            panic!("split")
        };
        assert_eq!(children.len(), 3, "un solo Split con tres hijos");
        assert_eq!(
            *sizes,
            vec![Size::Weight(1), Size::Weight(1), Size::Weight(1)]
        );
        assert_eq!(
            tres.slot_ids(),
            vec![SlotId(1), SlotId(2), SlotId(3)],
            "y el nuevo entra JUNTO al que se partió, no al final"
        );
    }

    /// En el OTRO eje sigue envolviendo: un corte perpendicular no puede
    /// entrar en la fila de sus hermanos.
    #[test]
    fn partir_en_el_otro_eje_sigue_anidando() {
        let arbol = b(1).split_slot(SlotId(1), Dir::Vertical, &b(2));
        let cruz = arbol.split_slot(SlotId(2), Dir::Horizontal, &b(3));
        let Node::Split { children, dir, .. } = &cruz else {
            panic!("split")
        };
        assert_eq!(*dir, Dir::Vertical);
        assert_eq!(children.len(), 2, "el de fuera sigue teniendo dos hijos");
        assert!(
            matches!(&children[1], Node::Split { dir, .. } if *dir == Dir::Horizontal),
            "y el corte nuevo va DENTRO del que se partió"
        );
    }

    /// Un hueco de tamaño FIJO se parte por dentro, no se une a sus hermanos:
    /// su tamaño es cromo acoplado, y meter otro hijo en esa fila le robaría
    /// el sitio a lo que hay al lado.
    #[test]
    fn partir_un_hueco_fijo_no_se_une_a_sus_hermanos() {
        let arbol = Node::Split {
            dir: Dir::Vertical,
            sizes: vec![Size::Weight(1), Size::Fixed(8)],
            children: vec![b(1), b(2)],
        };
        let partido = arbol.split_slot(SlotId(2), Dir::Vertical, &b(3));
        let Node::Split {
            children, sizes, ..
        } = &partido
        else {
            panic!("split")
        };
        assert_eq!(children.len(), 2, "sigue habiendo dos hijos arriba");
        assert_eq!(sizes[1], Size::Fixed(8), "y el fijo conserva su tamaño");
        assert_eq!(children[1].slot_ids(), vec![SlotId(2), SlotId(3)]);
    }

    /// Partir una PESTAÑA parte lo que estás mirando, no reorganiza sus
    /// hermanas: el corte va dentro de la pestaña, no alrededor del grupo.
    #[test]
    fn partir_una_pestana_corta_dentro_de_ella() {
        let arbol = Node::Tabs {
            children: vec![b(1), b(2)],
            active: 1,
        };
        let nuevo = arbol.split_slot(SlotId(2), Dir::Horizontal, &b(9));
        let Node::Tabs { children, active } = &nuevo else {
            panic!("sigue siendo un grupo de pestañas")
        };
        assert_eq!(*active, 1, "la pestaña activa no se mueve");
        assert_eq!(children.len(), 2, "sigue habiendo DOS pestañas");
        assert_eq!(children[1].slot_ids(), vec![SlotId(2), SlotId(9)]);
    }

    #[test]
    fn el_arbol_round_trippea_en_toml() {
        use crate::layout::{Dir, KindId, Node, Size, SlotId};
        let arbol = Node::Split {
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
        let t = toml::to_string_pretty(&arbol).expect("serializa a TOML");
        println!("---\n{t}\n---");
        let vuelta: Node = toml::from_str(&t).expect("vuelve de TOML");
        assert_eq!(vuelta, arbol);
    }

    /// Cerrar un hueco deja al hermano ocupando el sitio de los dos.
    #[test]
    fn cerrar_un_hueco_disuelve_el_split_de_dos() {
        let arbol = Node::split(Dir::Horizontal, vec![b(1), b(2)]);
        assert_eq!(arbol.close_slot(SlotId(2)), Some(b(1)));
    }

    /// Cerrar el ÚNICO hueco devuelve `None`: una pantalla sin nada no la
    /// decide el árbol.
    #[test]
    fn cerrar_el_unico_hueco_no_se_hace_solo() {
        assert_eq!(b(1).close_slot(SlotId(1)), None);
    }

    /// Al cerrar, el tamaño del hueco se va CON él: dejarlo desplazaría todos
    /// los pesos una posición y el reparto pasaría a ser otro sin avisar.
    #[test]
    fn cerrar_un_hueco_se_lleva_su_tamano() {
        let arbol = Node::Split {
            dir: Dir::Horizontal,
            sizes: vec![Size::Weight(3), Size::Weight(1), Size::Weight(1)],
            children: vec![b(1), b(2), b(3)],
        };
        let nuevo = arbol.close_slot(SlotId(1)).expect("quedan dos");
        let Node::Split { sizes, .. } = &nuevo else {
            panic!("sigue siendo un split")
        };
        assert_eq!(*sizes, vec![Size::Weight(1), Size::Weight(1)]);
    }

    /// Agrandar toca el peso del hueco enfocado, con tope.
    #[test]
    fn agrandar_sube_el_peso_hasta_el_tope() {
        let arbol = Node::split(Dir::Horizontal, vec![b(1), b(2)]);
        let mut a = arbol;
        for _ in 0..20 {
            a = a.resize(SlotId(1), 1);
        }
        let Node::Split { sizes, .. } = &a else {
            panic!("split")
        };
        assert_eq!(sizes[0], Size::Weight(10), "no crece sin fin");
    }

    /// Un hijo FIJO SÍ se agranda desde #227, en celdas: era lo que dejaba el
    /// sidebar atascado en el ancho con el que se abría.
    ///
    /// Lo que protege a la barra de estado —el otro hijo fijo que hay en la
    /// pantalla— no es esta función: es que `layout.grow` solo nombra al hueco
    /// CON EL FOCO, y el kind `status` no es enfocable. Un tope aquí por el
    /// tamaño del hijo sería adivinar cuál de los dos fijos es un sidebar.
    #[test]
    fn agrandar_mueve_un_hijo_fijo_en_celdas() {
        let arbol = Node::Split {
            dir: Dir::Vertical,
            sizes: vec![Size::Weight(1), Size::Fixed(1)],
            children: vec![b(1), Node::slot(SlotId(2), KindId::new("status"))],
        };
        let nuevo = arbol.resize(SlotId(2), 3);
        let Node::Split { sizes, .. } = &nuevo else {
            panic!("split")
        };
        assert_eq!(sizes[1], Size::Fixed(7));
    }

    /// Igualar devuelve los pesos a uno y deja los fijos en paz.
    #[test]
    fn igualar_solo_toca_los_ponderados() {
        let arbol = Node::Split {
            dir: Dir::Vertical,
            sizes: vec![Size::Weight(7), Size::Fixed(2), Size::Weight(3)],
            children: vec![b(1), b(2), b(3)],
        };
        let nuevo = arbol.equalize(SlotId(1));
        let Node::Split { sizes, .. } = &nuevo else {
            panic!("split")
        };
        assert_eq!(
            *sizes,
            vec![Size::Weight(1), Size::Fixed(2), Size::Weight(1)]
        );
    }

    /// Dos huecos con el mismo id es incoherente, y el árbol sabe decirlo.
    #[test]
    fn los_ids_repetidos_se_detectan() {
        let arbol = Node::Split {
            dir: Dir::Vertical,
            sizes: vec![Size::Weight(1), Size::Weight(1)],
            children: vec![
                Node::slot(SlotId(1), KindId::browser()),
                Node::slot(SlotId(1), KindId::browser()),
            ],
        };
        assert_eq!(arbol.duplicate_slot_ids(), vec![SlotId(1)]);
    }

    /// Un dock a la izquierda entra en el `Split` HORIZONTAL que ya existe, no
    /// alrededor del árbol entero: si envolviera la raíz, la barra de estado y
    /// la franja de tareas se quedarían a la DERECHA del sidebar en vez de
    /// debajo de los listados.
    #[test]
    fn dock_izquierda_entra_en_el_split_del_cuerpo() {
        let cuerpo = Node::split(
            Dir::Horizontal,
            vec![
                Node::slot(SlotId(1), KindId::browser()),
                Node::slot(SlotId(2), KindId::browser()),
            ],
        );
        let raiz = Node::Split {
            dir: Dir::Vertical,
            sizes: vec![Size::Weight(1), Size::Fixed(1)],
            children: vec![cuerpo, Node::slot(SlotId(4), KindId::new("status"))],
        };
        let con = raiz.dock(
            SlotId(1),
            Edge::Left,
            Size::Fixed(16),
            &Node::slot(SlotId(9), KindId::new("places")),
        );
        let Node::Split { children, .. } = &con else {
            panic!("la raíz sigue siendo un Split");
        };
        let Node::Split {
            children: cuerpo,
            sizes,
            dir,
        } = &children[0]
        else {
            panic!("el cuerpo sigue siendo un Split");
        };
        assert_eq!(*dir, Dir::Horizontal);
        assert_eq!(cuerpo.len(), 3);
        assert_eq!(cuerpo[0].first_slot_id(), Some(SlotId(9)));
        assert_eq!(sizes[0], Size::Fixed(16));
        // Y la barra de estado NO se movió: sigue siendo hija de la raíz.
        assert_eq!(children[1].first_slot_id(), Some(SlotId(4)));
    }

    /// A la derecha, al final del mismo split.
    #[test]
    fn dock_derecha_va_al_final() {
        let arbol = Node::split(
            Dir::Horizontal,
            vec![
                Node::slot(SlotId(1), KindId::browser()),
                Node::slot(SlotId(2), KindId::browser()),
            ],
        );
        let con = arbol.dock(
            SlotId(1),
            Edge::Right,
            Size::Weight(1),
            &Node::slot(SlotId(9), KindId::new("viewer")),
        );
        let Node::Split { children, .. } = &con else {
            panic!("split")
        };
        assert_eq!(children.len(), 3);
        assert_eq!(children[2].first_slot_id(), Some(SlotId(9)));
    }

    /// REGRESIÓN (captura del 2026-09-21): un PESO se mide contra los pesos
    /// hermanos, no en absoluto. Arrastrar el borde entre dos listados los
    /// deja en 49/51; un visor que entra con `Weight(1)` se quedaba con
    /// 1/101 del sitio libre — una barrita de un píxel que no se ve. Entra
    /// con la MEDIA de los pesos de sus hermanos, que es lo que `Weight(1)`
    /// significa en un reparto de unos.
    #[test]
    fn un_peso_que_entra_se_mide_contra_sus_hermanos() {
        let arbol = Node::Split {
            dir: Dir::Horizontal,
            children: vec![
                Node::slot(SlotId(5), KindId::new("places")),
                Node::slot(SlotId(1), KindId::browser()),
                Node::slot(SlotId(2), KindId::browser()),
            ],
            sizes: vec![Size::Fixed(16), Size::Weight(49), Size::Weight(51)],
        };
        let con = arbol.dock(
            SlotId(1),
            Edge::Right,
            Size::Weight(1),
            &Node::slot(SlotId(9), KindId::new("viewer")),
        );
        let Node::Split { sizes, .. } = &con else {
            panic!("split")
        };
        assert_eq!(sizes.last(), Some(&Size::Weight(50)), "{sizes:?}");
        // Un fijo no se toca: su número es de celdas, no de proporción.
        let con = arbol.dock(
            SlotId(1),
            Edge::Right,
            Size::Fixed(30),
            &Node::slot(SlotId(9), KindId::new("metadata")),
        );
        let Node::Split { sizes, .. } = &con else {
            panic!("split")
        };
        assert_eq!(sizes.last(), Some(&Size::Fixed(30)));
    }

    /// Un panel acoplado ABAJO entra por ENCIMA de la franja de tareas y de
    /// la barra de estado, no debajo (captura del 2026-09-21): el registro y
    /// procesos salían por debajo de la barra de estado. En VS Code el panel
    /// de abajo está siempre encima de la barra; y en el terminal, la barra
    /// de estado tiene que ser la última fila.
    #[test]
    fn abajo_entra_por_encima_de_la_barra_de_estado() {
        let arbol = Node::Split {
            dir: Dir::Vertical,
            children: vec![
                Node::slot(SlotId(1), KindId::browser()),
                Node::slot(SlotId(3), KindId::new("tasks")),
                Node::slot(SlotId(4), KindId::new("status")),
            ],
            sizes: vec![Size::Weight(1), Size::Auto, Size::Fixed(1)],
        };
        let con = arbol.dock(
            SlotId(1),
            Edge::Bottom,
            Size::Fixed(12),
            &Node::slot(SlotId(9), KindId::new("log")),
        );
        let Node::Split {
            children, sizes, ..
        } = &con
        else {
            panic!("split")
        };
        let ids: Vec<_> = children.iter().filter_map(Node::first_slot_id).collect();
        assert_eq!(ids, [SlotId(1), SlotId(9), SlotId(3), SlotId(4)]);
        assert_eq!(sizes[1], Size::Fixed(12), "el tamaño va con su hijo");
    }

    /// Los paneles de un mismo borde se AGRUPAN en pestañas (spec 2026-09-21,
    /// fase F): el segundo panel a la derecha no abre otra columna, se une a
    /// la del primero y queda delante; el tamaño es el del grupo. Un listado
    /// no se agrupa nunca, y cerrar una pestaña deshace el grupo.
    #[test]
    fn los_paneles_de_un_mismo_borde_se_agrupan_en_pestanas() {
        let cuerpo = Node::Split {
            dir: Dir::Horizontal,
            children: vec![
                Node::slot(SlotId(1), KindId::browser()),
                Node::slot(SlotId(2), KindId::browser()),
            ],
            sizes: vec![Size::Weight(1), Size::Weight(1)],
        };
        let hoja = |id: u32, k: &str| Node::slot(SlotId(id), KindId::new(k));
        // El primero: una columna, como siempre (su vecino es un listado).
        let uno = cuerpo.dock_grouped(
            SlotId(1),
            Edge::Right,
            Size::Fixed(30),
            &hoja(7, "timeline"),
        );
        let Node::Split { children, .. } = &uno else {
            panic!("split")
        };
        assert_eq!(children.len(), 3);
        // El segundo: pestaña del primero, delante, y el grupo toma el
        // sitio del que más pide — el visor es proporcional, y en las
        // treinta columnas del primero no se leería.
        let dos = uno.dock_grouped(SlotId(1), Edge::Right, Size::Weight(1), &hoja(9, "viewer"));
        let Node::Split {
            children, sizes, ..
        } = &dos
        else {
            panic!("split")
        };
        assert_eq!(children.len(), 3, "no abre otra columna");
        assert_eq!(
            dos.tabs_of(SlotId(9)),
            Some((vec![SlotId(7), SlotId(9)], 1))
        );
        assert_eq!(sizes[2], Size::Weight(1));
        // El tercero se une al mismo grupo, y un fijo no le quita el peso.
        let tres = dos.dock_grouped(
            SlotId(1),
            Edge::Right,
            Size::Fixed(30),
            &hoja(8, "metadata"),
        );
        assert_eq!(
            tres.tabs_of(SlotId(8)),
            Some((vec![SlotId(7), SlotId(9), SlotId(8)], 2))
        );
        // Abajo, por encima de la barra de estado, igual.
        let raiz = Node::Split {
            dir: Dir::Vertical,
            children: vec![tres, hoja(4, "status")],
            sizes: vec![Size::Weight(1), Size::Fixed(1)],
        };
        let r = raiz
            .dock_grouped(SlotId(1), Edge::Bottom, Size::Fixed(12), &hoja(10, "log"))
            .dock_grouped(
                SlotId(1),
                Edge::Bottom,
                Size::Fixed(8),
                &hoja(11, "processes"),
            );
        assert_eq!(
            r.tabs_of(SlotId(11)),
            Some((vec![SlotId(10), SlotId(11)], 1))
        );
        // Dos fijos: el mayor, que el registro no encoja a las ocho filas
        // de los procesos.
        let Node::Split { sizes, .. } = &r else {
            panic!("split")
        };
        assert_eq!(sizes[1], Size::Fixed(12));
        // Cerrar una pestaña de un grupo de dos lo deshace: vuelve la hoja.
        let cerrado = r.close_slot(SlotId(11)).expect("cerrable");
        assert_eq!(cerrado.tabs_of(SlotId(10)), None);
        // Y `dock` a secas sigue sin agrupar: es el de las disposiciones
        // escritas a mano y los presets.
        let suelto = dos.dock(
            SlotId(1),
            Edge::Right,
            Size::Fixed(30),
            &hoja(8, "metadata"),
        );
        assert_eq!(suelto.tabs_of(SlotId(8)), None);
    }

    /// Sin ancestro en el eje pedido, se ENVUELVE. Un solo pane es el caso
    /// real: tras cerrar uno, el cuerpo puede ser una hoja suelta.
    #[test]
    fn sin_ancestro_en_el_eje_se_envuelve() {
        let arbol = Node::slot(SlotId(1), KindId::browser());
        let con = arbol.dock(
            SlotId(1),
            Edge::Left,
            Size::Fixed(16),
            &Node::slot(SlotId(9), KindId::new("places")),
        );
        let Node::Split {
            dir,
            children,
            sizes,
        } = &con
        else {
            panic!("envuelto en Split")
        };
        assert_eq!(*dir, Dir::Horizontal);
        assert_eq!(children[0].first_slot_id(), Some(SlotId(9)));
        assert_eq!(children[1].first_slot_id(), Some(SlotId(1)));
        assert_eq!(sizes, &vec![Size::Fixed(16), Size::Weight(1)]);
    }

    /// Un ancla dentro de una `Tabs` acopla FUERA del grupo: un sidebar que
    /// desaparece al cambiar de pestaña no es un sidebar.
    #[test]
    fn con_el_ancla_en_una_pestana_el_acople_va_fuera_del_grupo() {
        let arbol = Node::split(
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
        let con = arbol.dock(
            SlotId(1),
            Edge::Left,
            Size::Fixed(16),
            &Node::slot(SlotId(9), KindId::new("places")),
        );
        let Node::Split { children, .. } = &con else {
            panic!("split")
        };
        assert_eq!(children.len(), 3);
        assert_eq!(children[0].first_slot_id(), Some(SlotId(9)));
        assert!(matches!(children[1], Node::Tabs { .. }));
    }

    /// Un ancla que no está en el árbol no inventa nada.
    #[test]
    fn un_ancla_que_no_existe_deja_el_arbol_intacto() {
        let arbol = Node::slot(SlotId(1), KindId::browser());
        let con = arbol.dock(
            SlotId(77),
            Edge::Left,
            Size::Fixed(16),
            &Node::slot(SlotId(9), KindId::new("places")),
        );
        assert_eq!(con, arbol);
    }

    /// Y deshacerlo es `close_slot`, que ya existe: el `Split` de un solo hijo
    /// se disuelve y el árbol vuelve a ser el de antes. Es lo que hace que el
    /// toggle sea reversible de verdad y no deje un Split degenerado por cada
    /// vez que alguien abrió y cerró el sidebar.
    #[test]
    fn undock_es_close_slot_y_devuelve_el_arbol_de_antes() {
        let arbol = Node::split(
            Dir::Horizontal,
            vec![
                Node::slot(SlotId(1), KindId::browser()),
                Node::slot(SlotId(2), KindId::browser()),
            ],
        );
        let con = arbol.dock(
            SlotId(1),
            Edge::Left,
            Size::Fixed(16),
            &Node::slot(SlotId(9), KindId::new("places")),
        );
        assert_eq!(con.close_slot(SlotId(9)), Some(arbol));
    }

    /// El ancho del primer hijo de un `Split`, para los tests de `resize`.
    fn ancho(n: &Node) -> Size {
        match n {
            Node::Split { sizes, .. } => sizes[0],
            _ => panic!("split"),
        }
    }

    fn con_sidebar(ancho: u16) -> Node {
        Node::Split {
            dir: Dir::Horizontal,
            children: vec![
                Node::slot(SlotId(5), KindId::new("places")),
                Node::slot(SlotId(1), KindId::browser()),
            ],
            sizes: vec![Size::Fixed(ancho), Size::Weight(1)],
        }
    }

    /// #227: un hijo FIJO —el ancho del sidebar— se mueve en CELDAS. Antes
    /// `resize` solo tocaba pesos, así que el panel de sitios no se podía
    /// ensanchar con el teclado y los presets con sidebar nacían atascados.
    #[test]
    fn un_hijo_fijo_se_mueve_en_celdas() {
        let arbol = con_sidebar(16);
        assert_eq!(ancho(&arbol.resize(SlotId(5), 1)), Size::Fixed(18));
        assert_eq!(ancho(&arbol.resize(SlotId(5), -1)), Size::Fixed(14));
    }

    /// El tope de abajo existe para que no se pueda dejar en cero: un panel de
    /// ancho cero no se ve y no hay forma de volver a agrandarlo.
    #[test]
    fn un_hijo_fijo_no_baja_de_dos_ni_pasa_de_cien() {
        assert_eq!(ancho(&con_sidebar(2).resize(SlotId(5), -1)), Size::Fixed(2));
        assert_eq!(
            ancho(&con_sidebar(100).resize(SlotId(5), 1)),
            Size::Fixed(100)
        );
    }

    /// Y un hijo PONDERADO sigue haciendo exactamente lo de antes.
    #[test]
    fn un_hijo_ponderado_no_cambia_de_comportamiento() {
        let arbol = Node::split(
            Dir::Horizontal,
            vec![
                Node::slot(SlotId(1), KindId::browser()),
                Node::slot(SlotId(2), KindId::browser()),
            ],
        );
        assert_eq!(ancho(&arbol.resize(SlotId(1), 1)), Size::Weight(2));
        assert_eq!(ancho(&arbol.resize(SlotId(1), -1)), Size::Weight(1));
    }

    /// El `orthodox` de siempre: dos listados lado a lado sobre las filas de
    /// cromo.
    fn ortodoxo() -> Node {
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

    /// ADR 0138: soltar a un lado reparte con el destino a partes iguales, y
    /// en el centro se une como pestaña. Nada se pierde.
    #[test]
    fn mover_un_hueco_lo_suelta_al_lado_o_como_pestana() {
        let t = ortodoxo();
        // El 1 debajo del 2: el reparto horizontal se disuelve, y el 2 se
        // parte en vertical DENTRO de su sitio — no como hermano en la raíz,
        // que es la del cromo y no se gira: así `flip` lo deshace.
        let abajo = t.move_slot(SlotId(1), SlotId(2), DropZone::Bottom);
        let Node::Split { children, .. } = &abajo else {
            panic!("raíz")
        };
        assert_eq!(
            children[0],
            Node::Split {
                dir: Dir::Vertical,
                children: vec![b(2), b(1)],
                sizes: vec![Size::Weight(1); 2],
            }
        );
        assert_eq!(children.len(), 3, "el cromo sigue igual");
        assert_eq!(
            abajo.flip(SlotId(1)),
            t.move_slot(SlotId(1), SlotId(2), DropZone::Right),
            "girar lo que se soltó abajo es soltarlo a la derecha"
        );
        // Un tercero a la derecha del 1 entra como HERMANO: tercios.
        let tres = Node::split(Dir::Horizontal, vec![b(1), b(2), b(5)]);
        let movido = tres.move_slot(SlotId(5), SlotId(1), DropZone::Right);
        let Node::Split {
            children, sizes, ..
        } = &movido
        else {
            panic!("split")
        };
        assert_eq!(children, &vec![b(1), b(5), b(2)]);
        assert_eq!(sizes, &vec![Size::Weight(1); 3]);
        // En el centro: pestaña del destino, delante.
        let centro = t.move_slot(SlotId(1), SlotId(2), DropZone::Center);
        assert_eq!(
            centro.tabs_of(SlotId(1)),
            Some((vec![SlotId(2), SlotId(1)], 1))
        );
        // Los mismos huecos, siempre.
        for m in [&abajo, &movido, &centro] {
            let mut ids = m.slot_ids();
            ids.sort_unstable();
            assert!(m.duplicate_slot_ids().is_empty());
            assert!(ids.windows(2).all(|w| w[0] != w[1]));
        }
    }

    /// Soltar al lado de una pestaña parte el GRUPO, no lo invade.
    #[test]
    fn soltar_junto_a_una_pestana_parte_su_grupo() {
        let grupo = Node::Tabs {
            children: vec![b(1), b(2)],
            active: 0,
        };
        let t = Node::split(Dir::Horizontal, vec![grupo.clone(), b(3)]);
        let m = t.move_slot(SlotId(3), SlotId(2), DropZone::Top);
        assert_eq!(
            m,
            Node::Split {
                dir: Dir::Vertical,
                children: vec![b(3), grupo],
                sizes: vec![Size::Weight(1); 2],
            }
        );
    }

    /// Lo que no se mueve: a sí mismo, el cromo, el único hueco, un id que
    /// no está.
    #[test]
    fn mover_lo_que_no_se_mueve_no_cambia_nada() {
        let t = ortodoxo();
        assert_eq!(t.move_slot(SlotId(1), SlotId(1), DropZone::Left), t);
        assert_eq!(t.move_slot(SlotId(4), SlotId(1), DropZone::Top), t);
        assert_eq!(t.move_slot(SlotId(1), SlotId(4), DropZone::Top), t);
        assert_eq!(t.move_slot(SlotId(9), SlotId(1), DropZone::Top), t);
        assert_eq!(b(1).move_slot(SlotId(1), SlotId(2), DropZone::Left), b(1));
    }

    /// El borde entre el segundo listado y los detalles separa el CUERPO de
    /// los detalles: la pareja se mide entera, y arrastrarlo mueve los
    /// detalles aunque el listado sea el último de su propio reparto.
    #[test]
    fn el_borde_entre_primos_mueve_su_pareja() {
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
        assert_eq!(
            t.border_pair(SlotId(2), SlotId(1)),
            None,
            "el orden importa"
        );
        // Cuerpo de 100 celdas y detalles de 50: el borde a 120 deja 30.
        let m = t.drag_border_between(SlotId(2), SlotId(7), 120.0 / 150.0, 150);
        let Node::Split { sizes, .. } = &m else {
            panic!("split")
        };
        assert_eq!(sizes[1], Size::Fixed(30));
    }

    /// Arrastrar el borde entre dos ponderados no aplasta a un tercero: la
    /// pareja conserva su suma.
    #[test]
    fn arrastrar_una_pareja_no_aplasta_al_tercero() {
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
        assert_eq!(w[0] + w[1], w[2] * 2, "la pareja sigue sumando dos tercios");
        assert_eq!(w[2] * 3, total);
        assert!(w[0] < w[1]);
    }

    /// La zona bajo el puntero, en celdas: la regla del cuarto, como la
    /// ventana; y la parte que se resalta.
    #[test]
    fn la_zona_de_soltar_sale_del_cuarto_mas_cercano() {
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

    /// Junto a un panel de ancho fijo, lo soltado entra como HERMANO con
    /// peso: partir el fijo por dentro le daría ocho columnas a un listado.
    #[test]
    fn soltar_junto_a_un_fijo_entra_como_hermano() {
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
        assert_eq!(sizes[0], Size::Fixed(16), "los sitios conservan su ancho");
        assert_eq!(children[1], b(2));
        assert!(matches!(sizes[1], Size::Weight(_)));
    }

    /// El centro solo junta familias iguales (ADR 0134): un listado no
    /// entra en las pestañas de los sitios, ni un panel en las de un
    /// listado.
    #[test]
    fn el_centro_no_mezcla_listados_y_paneles() {
        let t = Node::Split {
            dir: Dir::Horizontal,
            children: vec![Node::slot(SlotId(7), KindId::new("places")), b(1), b(2)],
            sizes: vec![Size::Fixed(16), Size::Weight(1), Size::Weight(1)],
        };
        assert_eq!(t.move_slot(SlotId(1), SlotId(7), DropZone::Center), t);
        assert_eq!(t.move_slot(SlotId(7), SlotId(1), DropZone::Center), t);
        assert_ne!(t.move_slot(SlotId(1), SlotId(2), DropZone::Center), t);
    }

    /// ADR 0138: girar pasa lado a lado a uno encima del otro y vuelve; no
    /// gira el reparto del cromo, y un fijo pasa a peso.
    #[test]
    fn girar_cambia_el_eje_del_reparto_interior() {
        let t = ortodoxo();
        let g = t.flip(SlotId(1));
        let Node::Split { children, .. } = &g else {
            panic!("raíz")
        };
        assert!(matches!(
            &children[0],
            Node::Split {
                dir: Dir::Vertical,
                ..
            }
        ));
        assert_eq!(g.flip(SlotId(2)), t, "girar dos veces es no girar");
        // Un solo listado sobre el cromo: nada que girar.
        let solo = Node::Split {
            dir: Dir::Vertical,
            children: vec![b(1), Node::slot(SlotId(4), KindId::new("status"))],
            sizes: vec![Size::Weight(1), Size::Fixed(1)],
        };
        assert_eq!(solo.flip(SlotId(1)), solo);
        // Solo la RACHA ponderada: los sitios siguen siendo una columna de
        // dieciséis, y los dos listados se apilan a su lado.
        let con_sitios = Node::Split {
            dir: Dir::Horizontal,
            children: vec![Node::slot(SlotId(7), KindId::new("places")), b(1), b(2)],
            sizes: vec![Size::Fixed(16), Size::Weight(1), Size::Weight(1)],
        };
        let g = con_sitios.flip(SlotId(1));
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
        // Y de vuelta: los sitios no pierden su ancho.
        let Node::Split { sizes, .. } = g.flip(SlotId(2)) else {
            panic!("split")
        };
        assert_eq!(sizes[0], Size::Fixed(16));
        // Una racha de uno no se gira, y la negativa NO sube a girar el
        // reparto de fuera.
        let solo_uno = Node::Split {
            dir: Dir::Horizontal,
            children: vec![Node::slot(SlotId(7), KindId::new("places")), b(1)],
            sizes: vec![Size::Fixed(30), Size::Weight(1)],
        };
        assert_eq!(solo_uno.flip(SlotId(1)), solo_uno);
        let anidado = Node::split(Dir::Vertical, vec![solo_uno.clone(), b(9)]);
        assert_eq!(anidado.flip(SlotId(1)), anidado);
    }
}
