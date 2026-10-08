//! Where each panel opens (ADR 0173): its NORMAL place, and the place the
//! reader last had it.
//!
//! One table for both frontends. The window sent every kind it did not
//! list to a thirty-cell column on the right, so the timeline, the
//! terminal and the disk map — lists and drawings that want WIDTH — came
//! out as narrow columns while the terminal put them at the bottom
//! (2026-10-08). And a panel the reader moved forgot it on closing: the
//! next opening put it back at the default.

use serde::{Deserialize, Serialize};

use super::{Dir, Edge, Node, Size, SlotId};

/// Where a panel sits: the edge of the body it is docked on and the room
/// it takes on that axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dock {
    /// The edge.
    pub edge: Edge,
    /// The room on the edge's axis.
    pub size: Size,
}

/// The place a panel kind opens at when the reader never put it anywhere —
/// VS Code's layout, where norte's panels have an equivalent:
///
/// - **left**, the side bar: what you navigate WITH — places, the tree;
/// - **right**, beside the editor: what describes the cursor — the viewer
///   (half the room, it is read) and the details;
/// - **bottom**, the panel: lists and drawings that want width — the
///   processes, the log, the terminal, the timeline and the disk map.
///
/// A kind nobody knows (a plugin's) goes to the right, at thirty cells.
#[must_use]
pub fn default_dock(kind: &str) -> Dock {
    let (edge, size) = match kind {
        "places" => (Edge::Left, Size::Fixed(16)),
        "tree" => (Edge::Left, Size::Fixed(24)),
        "viewer" => (Edge::Right, Size::Weight(1)),
        "metadata" => (Edge::Right, Size::Fixed(30)),
        "processes" => (Edge::Bottom, Size::Fixed(8)),
        "log" | "terminal" | "timeline" | "disk-map" => (Edge::Bottom, Size::Fixed(12)),
        _ => (Edge::Right, Size::Fixed(30)),
    };
    Dock { edge, size }
}

/// Where a panel of `kind` opens: where the reader last had it, if they
/// ever placed it, and otherwise its [`default_dock`] — with a bottom
/// default capped to a third of a short `column` ([`super::dock_rows`]).
/// A remembered size is the reader's and is not capped.
#[must_use]
pub fn dock_for(
    kind: &str,
    memory: &std::collections::BTreeMap<String, Dock>,
    column: Option<u16>,
) -> Dock {
    if let Some(d) = memory.get(kind) {
        return *d;
    }
    let d = default_dock(kind);
    match (d.edge, d.size) {
        (Edge::Bottom | Edge::Top, Size::Fixed(n)) => Dock {
            edge: d.edge,
            size: super::dock_rows(n, column),
        },
        _ => d,
    }
}

/// Every panel's place in `tree`, by kind, to remember (ADR 0173): what is
/// in the middle — a listing, a pane among others — has no place and is
/// left out. Listings and chrome are not panels.
#[must_use]
pub fn docks_in(tree: &Node) -> Vec<(String, Dock)> {
    tree.slot_ids()
        .into_iter()
        .filter_map(|s| {
            let kind = tree.kind_of(s)?.as_str().to_owned();
            if matches!(kind.as_str(), "browser" | "status" | "tasks") {
                return None;
            }
            Some((kind, dock_of(tree, s)?))
        })
        .collect()
}

/// Where `slot` sits in `tree`: the edge of the innermost `Split` whose
/// first or last pane is the slot — or the tab group holding it — with the
/// room it has there. Trailing chrome (tasks, status) does not count as
/// "last". `None` for a slot in the middle, or not in the tree.
#[must_use]
pub fn dock_of(tree: &Node, slot: SlotId) -> Option<Dock> {
    let mut found = None;
    walk(tree, slot, &mut found);
    found
}

fn walk(node: &Node, slot: SlotId, found: &mut Option<Dock>) -> bool {
    match node {
        Node::Slot { id, .. } => *id == slot,
        Node::Tabs { children, .. } => children.iter().any(|c| walk(c, slot, found)),
        Node::Split {
            dir,
            children,
            sizes,
        } => {
            let Some(pos) = children.iter().position(|c| walk(c, slot, found)) else {
                return false;
            };
            // The innermost one decides: a deeper `Split` already set it.
            if found.is_some() {
                return true;
            }
            let unit = &children[pos];
            let alone = match unit {
                Node::Slot { .. } => true,
                Node::Tabs { children, .. } => {
                    children.iter().all(|c| matches!(c, Node::Slot { .. }))
                }
                Node::Split { .. } => false,
            };
            if !alone || children.len() < 2 {
                return true;
            }
            let last = children.len()
                - 1
                - children
                    .iter()
                    .rev()
                    .take_while(|c| {
                        matches!(c, Node::Slot { kind, .. }
                            if matches!(kind.as_str(), "status" | "tasks"))
                    })
                    .count();
            let edge = match (dir, pos) {
                (Dir::Horizontal, 0) => Some(Edge::Left),
                (Dir::Vertical, 0) => Some(Edge::Top),
                (Dir::Horizontal, p) if p == last => Some(Edge::Right),
                (Dir::Vertical, p) if p == last => Some(Edge::Bottom),
                _ => None,
            };
            if let (Some(edge), Some(size)) = (edge, sizes.get(pos).copied()) {
                *found = Some(Dock { edge, size });
            }
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::KindId;

    fn s(i: u32, k: &str) -> Node {
        Node::slot(SlotId(i), KindId::new(k))
    }

    /// The study's table (2026-10-08): lists and drawings at the bottom,
    /// what you navigate with on the left, what describes the cursor on
    /// the right.
    #[test]
    fn the_normal_places() {
        for k in ["timeline", "terminal", "disk-map", "log", "processes"] {
            assert_eq!(default_dock(k).edge, Edge::Bottom, "{k}");
        }
        for k in ["places", "tree"] {
            assert_eq!(default_dock(k).edge, Edge::Left, "{k}");
        }
        for k in ["viewer", "metadata", "plugin:x:y"] {
            assert_eq!(default_dock(k).edge, Edge::Right, "{k}");
        }
    }

    /// A panel the reader placed goes back THERE; one never placed takes
    /// its default, a bottom one capped on a short column.
    #[test]
    fn the_memory_wins_over_the_default() {
        let mut memory = std::collections::BTreeMap::new();
        assert_eq!(
            dock_for("timeline", &memory, Some(24)),
            Dock {
                edge: Edge::Bottom,
                size: Size::Fixed(8)
            }
        );
        memory.insert(
            "timeline".to_owned(),
            Dock {
                edge: Edge::Right,
                size: Size::Fixed(50),
            },
        );
        assert_eq!(
            dock_for("timeline", &memory, Some(24)),
            Dock {
                edge: Edge::Right,
                size: Size::Fixed(50)
            }
        );
    }

    /// Where a panel is, read back from the tree: the timeline moved to the
    /// right at 40, the log at the bottom above the status bar, a panel in
    /// a tab group, a listing in the middle (nothing).
    #[test]
    fn a_panels_place_is_read_back_from_the_tree() {
        let tree = Node::Split {
            dir: Dir::Vertical,
            sizes: vec![Size::Weight(1), Size::Fixed(10), Size::Fixed(1)],
            children: vec![
                Node::Split {
                    dir: Dir::Horizontal,
                    sizes: vec![
                        Size::Fixed(16),
                        Size::Weight(1),
                        Size::Weight(1),
                        Size::Fixed(40),
                    ],
                    children: vec![
                        Node::Tabs {
                            children: vec![s(5, "places"), s(6, "tree")],
                            active: 0,
                        },
                        s(1, "browser"),
                        s(2, "browser"),
                        s(9, "timeline"),
                    ],
                },
                s(7, "log"),
                s(8, "status"),
            ],
        };
        assert_eq!(
            dock_of(&tree, SlotId(9)),
            Some(Dock {
                edge: Edge::Right,
                size: Size::Fixed(40)
            })
        );
        assert_eq!(
            dock_of(&tree, SlotId(7)),
            Some(Dock {
                edge: Edge::Bottom,
                size: Size::Fixed(10)
            })
        );
        assert_eq!(dock_of(&tree, SlotId(6)).map(|d| d.edge), Some(Edge::Left));
        assert_eq!(dock_of(&tree, SlotId(2)), None, "a listing in the middle");
        assert_eq!(dock_of(&tree, SlotId(99)), None);
    }
}
