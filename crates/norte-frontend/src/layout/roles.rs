//! Roles and bindings: who is who, and whose view each slot is.

use std::collections::BTreeMap;

use super::{Follow, KindRegistry, LayoutDiagnostic, Node, Resolved, RoleId, SlotId};

/// The named pointers inside the tree.
///
/// `active` is the focus; `target` is where an operation that needs a
/// second place goes. They are resolved on every frame because they are
/// claims about what is on screen NOW, not saved layout properties.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Roles {
    map: BTreeMap<RoleId, SlotId>,
    /// Did a PERSON designate it?
    ///
    /// Distinguishing this matters because with two panes the target is
    /// assigned automatically — it is the other one, and nobody notices —
    /// and that default must NOT survive a split: whoever splits a pane
    /// ends up with three, and a target they did not choose marked on one
    /// of them is exactly the guesswork ADR 0058 D7 forbids.
    target_explicit: bool,
}

impl Roles {
    /// Who has role `role`, if anyone.
    #[must_use]
    pub fn get(&self, role: RoleId) -> Option<SlotId> {
        self.map.get(&role).copied()
    }

    /// Gives role `role` to `slot`. A `target` set through here is
    /// EXPLICIT: someone chose it, so it survives more candidates
    /// appearing.
    pub fn set(&mut self, role: RoleId, slot: SlotId) {
        if role == RoleId::Target {
            self.target_explicit = true;
        }
        self.map.insert(role, slot);
    }

    /// Removes role `role` from whoever had it.
    pub fn clear(&mut self, role: RoleId) {
        if role == RoleId::Target {
            self.target_explicit = false;
        }
        self.map.remove(&role);
    }

    /// Did someone choose the target, or did the engine assign it for lack
    /// of another?
    #[must_use]
    pub const fn target_is_explicit(&self) -> bool {
        self.target_explicit
    }

    /// Only the focus. Shorthand for startup and for tests.
    #[must_use]
    pub fn con_active(slot: SlotId) -> Self {
        let mut r = Self::default();
        r.set(RoleId::Active, slot);
        r
    }

    /// Leaves the roles consistent with what is on screen. Called after
    /// EVERY `resolve`.
    ///
    /// - `active` always becomes the FOCUS.
    /// - `target` is kept if it is still visible and eligible. If not, it is
    ///   relocated to the ONE other visible candidate; with zero or with
    ///   several it is left UNSET, and whoever needs it will ask for a
    ///   path.
    ///
    /// That last point is the rule, not a detail: with two panes the target
    /// is obvious and nobody notices the concept exists, but with several —
    /// or with one behind a tab — a copy toward whichever the engine
    /// tie-breaks to is silent data loss (ADR 0058 D7).
    pub fn reconcile(
        &mut self,
        tree: &Node,
        resolved: &Resolved,
        decls: &KindRegistry,
        focus: SlotId,
    ) {
        self.set(RoleId::Active, focus);
        let candidates: Vec<SlotId> = resolved
            .placements
            .iter()
            .map(|(id, _)| *id)
            .filter(|id| *id != focus)
            .filter(|id| {
                tree.kind_of(*id)
                    .is_some_and(|k| decls.holds_role(k, RoleId::Target))
            })
            .collect();
        // An EXPLICIT target survives as long as it is still a candidate.
        // The default-assigned one does not: as soon as there is more than
        // one candidate it stops being "the other one" and becomes a
        // guess.
        let current = self.get(RoleId::Target);
        let still_valid = current.is_some_and(|a| candidates.contains(&a));
        if still_valid && (self.target_explicit || candidates.len() == 1) {
            return;
        }
        if let [only] = candidates[..] {
            self.map.insert(RoleId::Target, only);
            self.target_explicit = false;
        } else {
            self.clear(RoleId::Target);
        }
    }
}

/// Whether the TARGET role deserves to be marked on screen, with these
/// slots placed.
///
/// Whether the role EXISTS and whether it is PAINTED are two questions, and
/// this is the second. With two slots the target is "the other one" and
/// nobody needs to be told: the mark would be noise in the usual case, and
/// a mark that always shows stops being read. From three on, a copy toward
/// whichever slot the engine tie-breaks to is silent data loss (ADR 0058
/// D7), and there the mark is the only thing that says so.
///
/// Lives here because both frontends used to answer it and already
/// disagreed: the terminal reserves it for three or more and the window
/// turned it on always.
///
/// ```
/// use norte_frontend::layout::target_worth_marking;
///
/// assert!(!target_worth_marking(1));
/// assert!(!target_worth_marking(2), "with two, the target is the other one");
/// assert!(target_worth_marking(3));
/// ```
#[must_use]
pub fn target_worth_marking(visible_slots: usize) -> bool {
    visible_slots > 2
}

/// Which slot slot `de` looks at. `None` = looks at nobody.
///
/// A `follows` to a slot that no longer exists degrades to following the
/// `active` role and leaves a [`LayoutDiagnostic::FollowRetargeted`]: it is
/// what was wanted almost always, and leaving it silently broken is a side
/// panel staring into the void with nothing saying so.
#[must_use]
pub fn resolve_follow(
    tree: &Node,
    de: SlotId,
    roles: &Roles,
    diags: &mut Vec<LayoutDiagnostic>,
) -> Option<SlotId> {
    match tree.bindings_of(de).and_then(|b| b.follows) {
        None => None,
        Some(Follow::Role(r)) => roles.get(r),
        Some(Follow::Slot(s)) => {
            if tree.slot_ids().contains(&s) {
                Some(s)
            } else {
                diags.push(LayoutDiagnostic::FollowRetargeted { slot: de });
                roles.get(RoleId::Active)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{Bindings, Dir, KindId, Params, Rect, resolve};

    fn reg() -> KindRegistry {
        KindRegistry::builtin()
    }
    fn browser(id: u32) -> Node {
        Node::slot(SlotId(id), KindId::browser())
    }
    fn painted(tree: Node) -> (Node, Resolved) {
        let res = resolve(Rect::new(0, 0, 100, 30), &tree, &reg());
        (tree, res)
    }
    fn split(children: Vec<Node>) -> Node {
        Node::split(Dir::Horizontal, children)
    }

    /// With two browsers, `target` is the other one. That is what makes the
    /// orthodox layout behave EXACTLY as it does today with the concept now
    /// present but invisible: `F5` copies to the other pane and nobody
    /// notices.
    #[test]
    fn with_two_browsers_the_destination_is_the_other() {
        let (tree, res) = painted(split(vec![browser(1), browser(2)]));
        let mut roles = Roles::default();
        roles.reconcile(&tree, &res, &reg(), SlotId(1));
        assert_eq!(roles.get(RoleId::Active), Some(SlotId(1)));
        assert_eq!(roles.get(RoleId::Target), Some(SlotId(2)));
    }

    /// With ONE single browser there is no target, and that is NOT a broken
    /// state: the operation that needs it will ask for a path.
    #[test]
    fn with_a_single_browser_there_is_no_destination() {
        let (tree, res) = painted(browser(1));
        let mut roles = Roles::default();
        roles.reconcile(&tree, &res, &reg(), SlotId(1));
        assert_eq!(roles.get(RoleId::Target), None);
    }

    /// The target gets HIDDEN (tab switch): the role relocates to the
    /// visible candidate. A target behind a tab is silent data loss.
    #[test]
    fn a_destination_that_hides_gets_relocated() {
        let (tree, res) = painted(split(vec![
            browser(1),
            Node::Tabs {
                children: vec![browser(2), browser(3)],
                active: 1,
            },
        ]));
        assert_eq!(res.hidden, vec![SlotId(2)], "2 is hidden");
        let mut roles = Roles::default();
        roles.set(RoleId::Target, SlotId(2));
        roles.reconcile(&tree, &res, &reg(), SlotId(1));
        assert_eq!(
            roles.get(RoleId::Target),
            Some(SlotId(3)),
            "relocates to the visible one"
        );
    }

    /// With THREE visible browsers and none designated, there is no
    /// default: two candidates do not tie-break themselves.
    #[test]
    fn with_several_candidates_there_is_no_default_destination() {
        let (tree, res) = painted(split(vec![browser(1), browser(2), browser(3)]));
        let mut roles = Roles::default();
        roles.reconcile(&tree, &res, &reg(), SlotId(1));
        assert_eq!(roles.get(RoleId::Target), None);
    }

    /// The DEFAULT-assigned target (there was a single candidate) does NOT
    /// survive a second one appearing: it then stops being "the other one"
    /// and becomes a guess. Uncovered by piloting the TUI in tmux — after
    /// splitting a pane, a target nobody had chosen showed up marked.
    #[test]
    fn the_default_destination_does_not_survive_a_third_pane() {
        let (tree, res) = painted(split(vec![browser(1), browser(2)]));
        let mut roles = Roles::default();
        roles.reconcile(&tree, &res, &reg(), SlotId(1));
        assert_eq!(roles.get(RoleId::Target), Some(SlotId(2)));
        assert!(!roles.target_is_explicit(), "the engine set it, not anyone");

        let (tree, res) = painted(split(vec![browser(1), browser(2), browser(3)]));
        roles.reconcile(&tree, &res, &reg(), SlotId(1));
        assert_eq!(
            roles.get(RoleId::Target),
            None,
            "with two candidates there is no target the engine can give"
        );
    }

    /// But a target designated BY HAND is respected even with several: the
    /// engine does not tie-break, the user does.
    #[test]
    fn a_hand_designated_destination_survives_reconciliation() {
        let (tree, res) = painted(split(vec![browser(1), browser(2), browser(3)]));
        let mut roles = Roles::default();
        roles.set(RoleId::Target, SlotId(3));
        roles.reconcile(&tree, &res, &reg(), SlotId(1));
        assert_eq!(roles.get(RoleId::Target), Some(SlotId(3)));
    }

    /// A kind that cannot take the role is not a candidate even if visible:
    /// `tasks` is never a copy's target.
    #[test]
    fn a_kind_without_that_role_is_not_a_candidate() {
        let (tree, res) = painted(split(vec![
            browser(1),
            Node::slot(SlotId(2), KindId::new("tasks")),
        ]));
        let mut roles = Roles::default();
        roles.reconcile(&tree, &res, &reg(), SlotId(1));
        assert_eq!(roles.get(RoleId::Target), None);
    }

    /// A broken `follows` degrades to following the `active` role and
    /// COUNTS it.
    #[test]
    fn a_follow_to_a_slot_that_does_not_exist_degrades_to_active() {
        let tree = Node::Slot {
            id: SlotId(1),
            kind: KindId::new("metadata"),
            params: Params::new(),
            bindings: Bindings {
                follows: Some(Follow::Slot(SlotId(99))),
            },
        };
        let mut diags = vec![];
        let target = resolve_follow(&tree, SlotId(1), &Roles::con_active(SlotId(1)), &mut diags);
        assert_eq!(target, Some(SlotId(1)));
        assert!(
            diags
                .iter()
                .any(|d| matches!(d, LayoutDiagnostic::FollowRetargeted { .. }))
        );
    }

    /// A `follows: Role(Active)` follows the focus, which is the useful
    /// default for a metadata panel or a docked preview.
    #[test]
    fn a_follow_to_the_active_role_follows_focus() {
        let tree = Node::Slot {
            id: SlotId(1),
            kind: KindId::new("metadata"),
            params: Params::new(),
            bindings: Bindings {
                follows: Some(Follow::Role(RoleId::Active)),
            },
        };
        let mut diags = vec![];
        let target = resolve_follow(&tree, SlotId(1), &Roles::con_active(SlotId(5)), &mut diags);
        assert_eq!(target, Some(SlotId(5)));
        assert!(diags.is_empty());
    }
}
