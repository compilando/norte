//! The window system: a tree of slots that frontends lay out from the same
//! pure code.
//!
//! norte's screen used to be hand-written — `draw` split the frame and
//! `pane_geometry`/`pane_list_rows` REPLICATED that arithmetic for the mouse
//! and pagination — and it was correct precisely because it was fixed: there
//! was no room for a fifth thing. What replaces it lives here.
//!
//! # Four concepts that do not overlap
//!
//! - **Slot** ([`Node::Slot`]) — where a pane goes, with its identity.
//! - **Kind** ([`KindId`]) — WHAT panel it is. A string, not an enum, so a
//!   plugin can contribute one.
//! - **Role** ([`RoleId`]) — WHO is who: `active` and `target`, pointers
//!   resolved on every frame.
//! - **Binding** ([`Bindings`]) — WHOSE view a slot is. Without this, a side
//!   panel is a box with nothing inside.
//!
//! Tabs ([`Node::Tabs`]) are not a separate concept: they are a kind of
//! node, and where they fall in the tree decides whether they are
//! workspaces, pane tabs, or half a screen alternating views.
//!
//! The decision and its discarded alternatives are in ADR 0058; the design
//! is in `docs/superpowers/specs/2026-08-17-layout-slots-tabs-design.md`.

mod by_slot;
pub mod config;
mod focus;
mod kinds;
pub mod presets;
mod resolve;
mod roles;
mod store;
mod tree;

pub use by_slot::BySlot;
pub use focus::{focus_next, focus_prev};
pub use kinds::{KindDecl, KindRegistry, panel_kind_id};
pub use resolve::{Resolved, border_span, has_room_to_split, keeps_on_screen, resolve};
pub use roles::{Roles, resolve_follow, target_worth_marking};
pub use store::SlotStore;
pub use tree::{
    Bindings, Dir, DropZone, Edge, Follow, KindId, Node, Params, Rect, RoleId, Size, SlotId,
};

/// What prevents using a layout.
///
/// Distinguished from the diagnostic the same way the keymap distinguishes
/// [`crate::keymap::KeymapError`] from [`crate::keymap::KeymapDiagnostic`]:
/// an error leaves the previous layout standing (or falls back to the
/// default preset on a cold start), a diagnostic fixes itself and is
/// COUNTED.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum LayoutError {
    /// Two slots with the same id. There is no guessing which one wins.
    #[error("two slots with the same id: {0:?}")]
    DuplicateSlotId(SlotId),
    /// A `Split` with no children lays out nothing.
    #[error("a `Split` with no children")]
    EmptySplit,
    /// A layout's name cannot carry a path inside it.
    #[error("invalid layout name: {0:?}")]
    BadName(String),
    /// There is no file with that name.
    #[error("no layout at {0}")]
    NotFound(String),
    /// The file is not valid TOML, or does not describe a tree.
    #[error("the layout could not be read: {0}")]
    Parse(String),
    /// A tree with no `browser` at all is not a layout: it is a screen with
    /// no listing, and the frontend that applies it is left with no pane to
    /// point at.
    #[error("the layout has no listing slot at all")]
    NoBrowser,
    /// Sizes are index-parallel to the children.
    #[error("{count} sizes for {children} children")]
    WeightsMismatch {
        /// How many sizes there were.
        count: usize,
        /// How many children there are.
        children: usize,
    },
}

/// What fixes itself and has to be COUNTED. `norte doctor` shows them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutDiagnostic {
    /// The active tab was out of range.
    ActiveClamped {
        /// The index it carried.
        was: usize,
        /// What it was clamped to.
        to: usize,
    },
    /// A weight of zero lays out nothing; it is raised to one.
    ZeroWeightRaised {
        /// The child's position within its `Split`.
        at: usize,
    },
    /// A `follows` pointed at a slot that does not exist; it switches to
    /// following the `active` role, which is what was wanted 95% of the
    /// time.
    FollowRetargeted {
        /// The slot whose binding was redirected.
        slot: SlotId,
    },
    /// Chrome set aside FOR THIS FRAME because, with it, not even one usable
    /// listing fit (#229). The tree is not touched: as the terminal grows it
    /// comes back.
    ///
    /// Nobody paints it today — the frontends do not read `diagnostics` —
    /// so "why isn't my sidebar there" still has no answer on screen: that
    /// is #232, together with the loose-window mark.
    ChromeSetAside {
        /// The slot that was set aside.
        slot: SlotId,
    },
}

/// Validates a tree before using it.
///
/// Only what is INCONSISTENT: what is clampable (an active tab out of
/// range, a weight of zero) is not an error, it is fixed during layout and
/// counted as a [`LayoutDiagnostic`].
///
/// # Errors
///
/// [`LayoutError`] if there are repeated ids, a `Split` with no children, or
/// sizes that are not index-parallel to the children.
pub fn validate(tree: &Node) -> Result<(), LayoutError> {
    if let Some(id) = tree.duplicate_slot_ids().first() {
        return Err(LayoutError::DuplicateSlotId(*id));
    }
    // No listing, no layout (#242). The rule is the same one
    // `layout.close-slot` already applies — "a screen with no listing at
    // all is not a layout, it is a hang with borders" — and here is where
    // it has to be applied: the frontend seeds a panel PER SLOT, so a tree
    // that calls the slot where the listing was `places` leaves none, and
    // the first side access panics in raw mode over the alternate screen.
    if !tree
        .slot_ids()
        .into_iter()
        .any(|id| tree.kind_of(id).is_some_and(|k| *k == KindId::browser()))
    {
        return Err(LayoutError::NoBrowser);
    }
    validate_shape(tree)
}

fn validate_shape(node: &Node) -> Result<(), LayoutError> {
    match node {
        Node::Split {
            children, sizes, ..
        } => {
            if children.is_empty() {
                return Err(LayoutError::EmptySplit);
            }
            if sizes.len() != children.len() {
                return Err(LayoutError::WeightsMismatch {
                    count: sizes.len(),
                    children: children.len(),
                });
            }
            for c in children {
                validate_shape(c)?;
            }
            Ok(())
        }
        Node::Tabs { children, .. } => {
            if children.is_empty() {
                return Err(LayoutError::EmptySplit);
            }
            for c in children {
                validate_shape(c)?;
            }
            Ok(())
        }
        Node::Slot { .. } => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tree_with_repeated_ids_is_not_used() {
        let tree = Node::Split {
            dir: Dir::Vertical,
            sizes: vec![Size::Weight(1), Size::Weight(1)],
            children: vec![
                Node::slot(SlotId(1), KindId::browser()),
                Node::slot(SlotId(1), KindId::browser()),
            ],
        };
        assert_eq!(
            validate(&tree),
            Err(LayoutError::DuplicateSlotId(SlotId(1)))
        );
    }

    /// Sizes are index-parallel: one short and layout would paint a slot
    /// somewhere wrong instead of failing.
    #[test]
    fn sizes_must_be_as_many_as_children() {
        let tree = Node::Split {
            dir: Dir::Horizontal,
            sizes: vec![Size::Weight(1)],
            children: vec![
                Node::slot(SlotId(1), KindId::browser()),
                Node::slot(SlotId(2), KindId::browser()),
            ],
        };
        assert_eq!(
            validate(&tree),
            Err(LayoutError::WeightsMismatch {
                count: 1,
                children: 2
            })
        );
    }

    /// A tree with no `browser` USED TO PASS validation, and then the
    /// frontend seeded the slot with the kind the tree asked for —
    /// overwriting the listing that was at that id — and the first side
    /// access panicked (#242). Persisted in the session, it panicked on
    /// EVERY startup.
    #[test]
    fn a_tree_without_a_listing_is_not_a_layout() {
        let tree = Node::Split {
            dir: Dir::Horizontal,
            sizes: vec![Size::Weight(1), Size::Weight(1)],
            children: vec![
                Node::slot(SlotId(1), KindId::new("places")),
                Node::slot(SlotId(4), KindId::new("status")),
            ],
        };
        assert_eq!(validate(&tree), Err(LayoutError::NoBrowser));
    }

    /// A listing inside a tab that is not currently shown STILL counts as a
    /// listing: the active tab changes with a key, and rejecting the tree
    /// based on where the focus was when it was saved would reject healthy
    /// layouts.
    #[test]
    fn a_listing_in_a_hidden_tab_counts() {
        let tree = Node::Tabs {
            active: 1,
            children: vec![
                Node::slot(SlotId(1), KindId::browser()),
                Node::slot(SlotId(2), KindId::new("viewer")),
            ],
        };
        assert_eq!(validate(&tree), Ok(()));
    }

    #[test]
    fn a_healthy_tree_validates() {
        let tree = Node::Split {
            dir: Dir::Horizontal,
            sizes: vec![Size::Weight(1), Size::Weight(1)],
            children: vec![
                Node::slot(SlotId(1), KindId::browser()),
                Node::slot(SlotId(2), KindId::browser()),
            ],
        };
        assert_eq!(validate(&tree), Ok(()));
    }
}
