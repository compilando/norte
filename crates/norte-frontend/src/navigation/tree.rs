//! The directory tree panel (#136): which branches are open and which row
//! is under the cursor.
//!
//! **Directories only.** A tree that also showed files would be a second
//! listing, worse than the one already alongside it: what this panel
//! answers is "how is this organized", and for that files are noise.
//!
//! **Lazy, for the same reason the local listing does not bring sizes
//! (#52):** unfolding a branch lists THAT directory and nothing more. A
//! tree that read itself whole on opening would take minutes on a big
//! `$HOME` and hours on a remote.
//!
//! The STATE and the decision of what needs requesting live here; requesting
//! it belongs to the run loop, which is the one holding the backend — the
//! same split as the docked viewer and the attributes sheet.

use std::collections::{BTreeMap, BTreeSet};

use norte_proto::VPath;

/// The kind that occupies a tree slot.
pub const KIND: &str = "tree";

/// A paintable tree row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// This row's directory.
    pub path: VPath,
    /// How many levels below the root (the root is 0).
    pub depth: usize,
    /// Is it unfolded.
    pub expanded: bool,
    /// Does it have children to show. `None` = not looked at yet.
    ///
    /// The three states are distinct for the reader: a branch that can be
    /// opened, a leaf that cannot, and one not yet known. Painting "leaf"
    /// on something that has not been read would be a made-up answer.
    pub children: Option<bool>,
}

/// A panel's directory tree.
#[derive(Debug, Default)]
pub struct Tree {
    /// What it hangs from.
    root: Option<VPath>,
    /// The DIRECTORY children of each directory already listed.
    child_dirs: BTreeMap<VPath, Vec<VPath>>,
    /// Which branches are unfolded.
    expanded_dirs: BTreeSet<VPath>,
    /// Where the cursor is, by position among the visible rows.
    cursor: usize,
    /// The directory [`Self::follow`] wants to leave under the cursor and
    /// that is NOT YET a row.
    ///
    /// Revealing a deep branch needs listing every level, and that is
    /// several turns of the run loop: without tracking the target, the
    /// cursor would stay on the last ancestor that did exist when it was
    /// requested. It is settled in [`Self::insert_children`], which is when
    /// new rows appear.
    revealing: Option<VPath>,
    /// The last directory [`Self::follow`] followed to.
    ///
    /// Used to NOT move the cursor again when the listing has not changed
    /// place: the funnel that follow goes through also runs on a refresh
    /// and on a click, and without this the cursor the reader had moved by
    /// hand to look at another branch would jump back every time they
    /// clicked in the listing.
    followed: Option<VPath>,
}

impl Tree {
    /// Anchors the tree at `root` (and empties it if it changes place).
    ///
    /// Changing root DISCARDS what was read: another tree's open branches
    /// say nothing about this one, and keeping them would make the panel
    /// show a mix of two places.
    pub fn anchor(&mut self, root: VPath) {
        if self.root.as_ref() == Some(&root) {
            return;
        }
        self.root = Some(root);
        self.child_dirs.clear();
        self.expanded_dirs.clear();
        self.cursor = 0;
        self.revealing = None;
        self.followed = None;
    }

    /// Anchors the tree NEAR `dir` and reveals it: at `home` if `dir` hangs
    /// from it, and if not at its provider's root.
    ///
    /// Anchoring AT `dir` itself — what it used to do — showed a single row
    /// when the directory has no subfolders, with the whole path as its
    /// name: a tree that shows nothing around itself is no use for moving
    /// around (capture of 2026-09-21). Hanging it from further up and
    /// revealing the branch is what VS Code's explorer does.
    ///
    /// ```
    /// use norte_frontend::tree::Tree;
    /// use norte_proto::VPath;
    /// let vp = |w: &str| VPath::parse(w).unwrap();
    /// let home = vp("file:///home/ana");
    /// let mut t = Tree::default();
    /// t.anchor_near(&vp("file:///home/ana/snapshots/2026"), &home);
    /// assert_eq!(t.root(), Some(&home));
    /// assert_eq!(t.revealing(), Some(&vp("file:///home/ana/snapshots/2026")));
    /// // Outside home, the provider's root.
    /// let mut t = Tree::default();
    /// t.anchor_near(&vp("file:///etc/ssh"), &home);
    /// assert_eq!(t.root(), Some(&vp("file:///")));
    /// // And on another provider, also its root.
    /// let mut t = Tree::default();
    /// t.anchor_near(&vp("mem:///r/a"), &home);
    /// assert_eq!(t.root(), Some(&vp("mem:///")));
    /// ```
    pub fn anchor_near(&mut self, dir: &VPath, home: &VPath) {
        let chain = Self::up_to_the_root(dir);
        let base = if chain.contains(home) {
            home.clone()
        } else {
            chain.last().cloned().unwrap_or_else(|| dir.clone())
        };
        self.anchor(base);
        self.follow(dir);
    }

    /// A branch could not be read.
    ///
    /// It is marked as read and EMPTY — otherwise it would be requested
    /// again on every turn, a request loop against a forbidden directory —
    /// unless it is the ROOT and the tree is following another directory:
    /// then the tree re-anchors at that directory. Happens with
    /// [`Self::anchor_near`] on a server that will not list `/`, or on an
    /// isolated system: a tree hanging from an unreadable root would show
    /// nothing.
    ///
    /// ```
    /// use norte_frontend::tree::Tree;
    /// use norte_proto::VPath;
    /// let vp = |w: &str| VPath::parse(w).unwrap();
    /// let mut t = Tree::default();
    /// t.anchor_near(&vp("mem:///home"), &vp("file:///home/ana"));
    /// assert_eq!(t.root(), Some(&vp("mem:///")));
    /// t.branch_unreadable(vp("mem:///"));
    /// assert_eq!(t.root(), Some(&vp("mem:///home")), "goes back to the listing");
    /// // Another unreadable branch is just marked empty.
    /// t.branch_unreadable(vp("mem:///home/closed"));
    /// assert_eq!(t.root(), Some(&vp("mem:///home")));
    /// ```
    pub fn branch_unreadable(&mut self, dir: VPath) {
        if self.root.as_ref() == Some(&dir)
            && let Some(followed) = self.followed.clone()
            && followed != dir
        {
            self.anchor(followed.clone());
            self.follow(&followed);
            return;
        }
        self.insert_children(dir, Vec::new());
    }

    /// Follows the listing alongside it: leaves `dir` under the cursor
    /// WITHOUT discarding what is open. Says whether it moved anything.
    ///
    /// It is the difference between a tree that helps and one that gets in
    /// the way. [`Self::anchor`] empties out — it has to, because branches
    /// of another root say nothing about this one — so re-anchoring on
    /// every `cd` would close the whole tree every time someone enters a
    /// folder.
    ///
    /// **The rule is one: what was read still holds as long as the new root
    /// is an ANCESTOR of the old one.** From that come the three cases:
    ///
    /// - `dir` hangs from the root: its ancestors are unfolded and the
    ///   cursor goes there. The root does not move and nothing is
    ///   discarded.
    /// - `dir` is ABOVE or alongside: the root climbs to the deepest common
    ///   ancestor and **everything is kept**, because every branch already
    ///   read still hangs from there. This is what keeps going up a level
    ///   (`nav.up`) and switching between two sibling panels with `Tab`
    ///   from closing the tree on every keystroke — it used to, and it was
    ///   the program's most used key.
    /// - Another provider or another machine: then yes, it anchors and
    ///   empties out. There is no common ancestor, and a tree showing one
    ///   place next to a listing showing another answers nothing.
    ///
    /// `dir` itself is NOT unfolded: whoever navigates there is already
    /// looking at its contents in the listing alongside it, and unfolding
    /// it would cost one more listing per step the reader takes.
    ///
    /// Following the SAME directory TWICE does not move the cursor again:
    /// this funnel is also where refreshes and clicks pass through, and
    /// without this the cursor the reader had moved by hand jumped back
    /// every time they clicked in the listing.
    ///
    /// ```
    /// use norte_frontend::tree::Tree;
    /// use norte_proto::VPath;
    /// let vp = |w: &str| VPath::parse(w).unwrap();
    /// let mut t = Tree::default();
    /// t.anchor(vp("mem:///r/a"));
    /// t.insert_children(vp("mem:///r/a"), vec![vp("mem:///r/a/y")]);
    /// t.follow(&vp("mem:///r/a/y"));
    /// assert_eq!(t.selected(), Some(vp("mem:///r/a/y")));
    /// // Going up a level moves the ROOT upward and keeps what was read:
    /// // the new root's listing is needed — the same trip `anchor` would
    /// // have requested — and with it everything that was open comes back.
    /// t.follow(&vp("mem:///r"));
    /// assert_eq!(t.root(), Some(&vp("mem:///r")));
    /// t.insert_children(vp("mem:///r"), vec![vp("mem:///r/a")]);
    /// assert!(t.rows().iter().any(|f| f.path == vp("mem:///r/a/y")));
    /// ```
    pub fn follow(&mut self, dir: &VPath) -> bool {
        let Some(root) = self.root.clone() else {
            self.anchor(dir.clone());
            self.followed = Some(dir.clone());
            return true;
        };
        let chain_dir = Self::up_to_the_root(dir);
        let chain_root = Self::up_to_the_root(&root);
        // The deepest COMMON ancestor: the first in `dir`'s chain — which
        // goes bottom-up — that is also in the root's. None means two
        // different providers, and then nothing that was read is of use.
        let Some(base) = chain_dir.iter().find(|p| chain_root.contains(p)).cloned() else {
            self.anchor(dir.clone());
            self.followed = Some(dir.clone());
            return true;
        };
        let same_place = self.followed.as_ref() == Some(dir);
        let before = (self.root.clone(), self.expanded_dirs.len());
        if base != root {
            // The root CLIMBS, and does not empty out: the new one is an
            // ancestor of the old, so every branch already read still
            // hangs from it. What is needed is to unfold the chain up to
            // the previous root, or what was open would stop being
            // visible — it stops being at depth zero, the only one that
            // unfolds on its own.
            self.root = Some(base.clone());
            for p in chain_root.iter().take_while(|p| **p != base) {
                self.expanded_dirs.insert(p.clone());
            }
        }
        // `dir`'s ANCESTORS down to the base; not `dir` itself.
        for p in chain_dir.iter().take_while(|p| **p != base).skip(1) {
            self.expanded_dirs.insert(p.clone());
        }
        self.followed = Some(dir.clone());
        let moved = before != (self.root.clone(), self.expanded_dirs.len());
        if same_place && !moved {
            // The listing has not moved place: the tree's cursor belongs to
            // the reader.
            return false;
        }
        self.revealing = Some(dir.clone());
        let cursor_before = self.cursor;
        self.settle_reveal();
        moved || self.cursor != cursor_before
    }

    /// `dir` and all its ancestors, from the deepest to the provider's
    /// root.
    fn up_to_the_root(dir: &VPath) -> Vec<VPath> {
        let mut out = vec![dir.clone()];
        let mut current = dir.clone();
        while let Some(parent) = current.parent() {
            out.push(parent.clone());
            current = parent;
        }
        out
    }

    /// The directory [`Self::follow`] asked for and that still has no row,
    /// if there is one.
    ///
    /// What is missing for it to exist is listing its ancestors, and
    /// [`Self::wants`] already requests that on its own.
    ///
    /// ```
    /// use norte_frontend::tree::Tree;
    /// use norte_proto::VPath;
    /// let vp = |w: &str| VPath::parse(w).unwrap();
    /// let mut t = Tree::default();
    /// t.anchor(vp("mem:///r"));
    /// t.insert_children(vp("mem:///r"), vec![vp("mem:///r/a")]);
    /// t.follow(&vp("mem:///r/a/y"));
    /// assert_eq!(t.revealing(), Some(&vp("mem:///r/a/y")));
    /// ```
    #[must_use]
    pub fn revealing(&self) -> Option<&VPath> {
        self.revealing.as_ref()
    }

    /// Puts the cursor on the branch [`Self::follow`] asked for, if it is
    /// already a row.
    ///
    /// And releases the target once it is known it will NOT appear: the
    /// parent is already listed and `dir` is not among its children. This
    /// really happens — a branch's listing is capped, so the needed child
    /// can be left out — and without this the target would stay set
    /// forever, paying for a row scan on every branch that arrives.
    fn settle_reveal(&mut self) {
        let Some(target) = self.revealing.clone() else {
            return;
        };
        if let Some(i) = self.rows().iter().position(|r| r.path == target) {
            self.cursor = i;
            self.revealing = None;
            return;
        }
        if let Some(parent) = target.parent()
            && let Some(children) = self.child_dirs.get(&parent)
            && !children.contains(&target)
        {
            self.revealing = None;
        }
    }

    /// Where it is anchored.
    #[must_use]
    pub fn root(&self) -> Option<&VPath> {
        self.root.as_ref()
    }

    /// Stores `dir`'s just-listed DIRECTORY children.
    ///
    /// The ORDER it arrives in is what gets painted: it is decided by
    /// whoever listed, with the same comparator as the listing alongside
    /// it. Reordering here would be a second criterion that drifts from
    /// the first as soon as someone changes one.
    pub fn insert_children(&mut self, dir: VPath, child_dirs: Vec<VPath>) {
        self.child_dirs.insert(dir, child_dirs);
        // New rows: one of them might be the one that was being revealed.
        self.settle_reveal();
    }

    /// Which directory needs listing to paint what is open, if any.
    ///
    /// One per turn, and the topmost one first: the run loop requests it,
    /// stores it with [`Self::insert_children`] and on the next turn this
    /// function says the next one. So a branch with a thousand children
    /// neither blocks the loop nor gets requested twice.
    #[must_use]
    pub fn wants(&self) -> Option<VPath> {
        let root = self.root.as_ref()?;
        if !self.child_dirs.contains_key(root) {
            return Some(root.clone());
        }
        self.rows()
            .into_iter()
            .find(|r| r.expanded && !self.child_dirs.contains_key(&r.path))
            .map(|r| r.path)
    }

    /// The visible rows, in paint order.
    #[must_use]
    pub fn rows(&self) -> Vec<Row> {
        let Some(root) = self.root.clone() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        self.push_rows(&root, 0, &mut out);
        out
    }

    fn push_rows(&self, dir: &VPath, depth: usize, out: &mut Vec<Row>) {
        let child_dirs = self.child_dirs.get(dir);
        let expanded = self.expanded_dirs.contains(dir) || depth == 0;
        out.push(Row {
            path: dir.clone(),
            depth,
            expanded,
            children: child_dirs.map(|h| !h.is_empty()),
        });
        if !expanded {
            return;
        }
        for h in child_dirs.into_iter().flatten() {
            self.push_rows(h, depth + 1, out);
        }
    }

    /// The row under the cursor, clamped to what there is.
    #[must_use]
    pub fn cursor(&self) -> usize {
        let n = self.rows().len();
        self.cursor.min(n.saturating_sub(1))
    }

    /// The directory under the cursor.
    #[must_use]
    pub fn selected(&self) -> Option<VPath> {
        let rows = self.rows();
        rows.get(self.cursor()).map(|r| r.path.clone())
    }

    /// Puts the cursor on a specific row, clamped to what there is.
    ///
    /// For the mouse: a click names a row by its INDEX, and the index can
    /// come from a frame taken before a branch's children arrived. It is
    /// clamped instead of rejected because whoever rejects is the
    /// GENERATION, which is the one that knows whether the painted tree is
    /// this one.
    pub fn set_cursor(&mut self, row: usize) {
        let n = self.rows().len();
        self.cursor = row.min(n.saturating_sub(1));
    }

    /// Goes up.
    pub const fn up(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    /// Goes down.
    pub fn down(&mut self) {
        let n = self.rows().len();
        self.cursor = (self.cursor + 1).min(n.saturating_sub(1));
    }

    /// Unfolds the branch under the cursor. The root is always unfolded.
    pub fn expand(&mut self) {
        if let Some(p) = self.selected() {
            self.expanded_dirs.insert(p);
        }
    }

    /// Folds the branch under the cursor.
    ///
    /// What was READ is kept: opening it again does not cost another trip,
    /// and a directory's content does not change by folding it.
    pub fn collapse(&mut self) {
        if let Some(p) = self.selected() {
            self.expanded_dirs.remove(&p);
        }
    }

    /// Folds or unfolds, depending on its state.
    pub fn toggle(&mut self) {
        let Some(p) = self.selected() else { return };
        if self.expanded_dirs.contains(&p) {
            self.expanded_dirs.remove(&p);
        } else {
            self.expanded_dirs.insert(p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vp(w: &str) -> VPath {
        VPath::parse(w).expect("wire")
    }

    fn with_root() -> Tree {
        let mut t = Tree::default();
        t.anchor(vp("mem:///r"));
        t
    }

    /// The first thing needed is the root, and once that is there, next is
    /// whatever the reader has opened: one per turn, top to bottom.
    #[test]
    fn asks_for_the_root_and_then_what_gets_opened() {
        let mut t = with_root();
        assert_eq!(t.wants(), Some(vp("mem:///r")));
        t.insert_children(vp("mem:///r"), vec![vp("mem:///r/a"), vp("mem:///r/b")]);
        assert_eq!(t.wants(), None, "nothing open, nothing to request");
        t.down();
        t.expand();
        assert_eq!(t.wants(), Some(vp("mem:///r/a")));
    }

    /// A directory already listed is not requested again even if it is
    /// folded and opened again: its content does not change by folding it.
    #[test]
    fn what_was_read_is_not_requested_again() {
        let mut t = with_root();
        t.insert_children(vp("mem:///r"), vec![vp("mem:///r/a")]);
        t.down();
        t.expand();
        t.insert_children(vp("mem:///r/a"), Vec::new());
        assert_eq!(t.wants(), None);
        t.collapse();
        t.expand();
        assert_eq!(t.wants(), None, "already known what is inside");
    }

    /// Rows come out in paint order, with their depth, and a folded branch
    /// hides its own.
    #[test]
    fn rows_carry_their_depth_and_folded_ones_do_not_show() {
        let mut t = with_root();
        t.insert_children(vp("mem:///r"), vec![vp("mem:///r/a")]);
        t.insert_children(vp("mem:///r/a"), vec![vp("mem:///r/a/x")]);
        assert_eq!(
            t.rows().len(),
            2,
            "the root and its child; the grandchild is folded"
        );
        t.down();
        t.expand();
        let rows = t.rows();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[2].depth, 2);
        assert_eq!(rows[2].path, vp("mem:///r/a/x"));
    }

    /// "No children" and "not looked at yet" are different, and the panel
    /// paints them differently: saying "leaf" about something unread is
    /// making up the answer.
    #[test]
    fn unread_and_childless_are_not_the_same() {
        let mut t = with_root();
        t.insert_children(vp("mem:///r"), vec![vp("mem:///r/a")]);
        assert_eq!(t.rows()[1].children, None, "nothing known about `a` yet");
        t.insert_children(vp("mem:///r/a"), Vec::new());
        assert_eq!(
            t.rows()[1].children,
            Some(false),
            "and now it is known: none"
        );
    }

    /// Changing root discards what was read: another tree's open branches
    /// say nothing about this one.
    #[test]
    fn changing_root_empties_what_was_read() {
        let mut t = with_root();
        t.insert_children(vp("mem:///r"), vec![vp("mem:///r/a")]);
        t.down();
        t.expand();
        t.anchor(vp("mem:///otro"));
        assert_eq!(t.rows().len(), 1, "only the new root");
        assert_eq!(t.wants(), Some(vp("mem:///otro")));
        assert_eq!(t.cursor(), 0);
    }

    /// Following the listing leaves the branch under the cursor and does
    /// NOT close what might be open elsewhere in the tree: re-anchoring on
    /// every `cd` was exactly what made keeping the panel open useless.
    #[test]
    fn following_reveals_the_branch_and_preserves_what_is_open() {
        let mut t = with_root();
        t.insert_children(vp("mem:///r"), vec![vp("mem:///r/a"), vp("mem:///r/b")]);
        // `b` stays open: it is the sibling that must not be closed.
        t.set_cursor(2);
        t.expand();
        t.insert_children(vp("mem:///r/b"), vec![vp("mem:///r/b/x")]);
        t.insert_children(vp("mem:///r/a"), vec![vp("mem:///r/a/y")]);

        t.follow(&vp("mem:///r/a/y"));

        let rows: Vec<VPath> = t.rows().into_iter().map(|r| r.path).collect();
        assert_eq!(
            rows,
            vec![
                vp("mem:///r"),
                vp("mem:///r/a"),
                vp("mem:///r/a/y"),
                vp("mem:///r/b"),
                vp("mem:///r/b/x"),
            ],
            "the ancestor unfolds and `b` stays open"
        );
        assert_eq!(t.selected(), Some(vp("mem:///r/a/y")));
        assert_eq!(t.revealing(), None, "already revealed");
    }

    /// **Going up a level moves the root UP and keeps what was read.**
    ///
    /// `nav.up` is one of the most-pressed keys of an orthodox manager, and
    /// it used to empty the whole tree: the new root did not hang from the
    /// old one, so it anchored. But the other way around it DID hang —
    /// everything already read is still under the new root — and throwing
    /// it away was free to avoid and cost another listing besides.
    #[test]
    fn following_upward_raises_the_root_without_emptying_it() {
        let mut t = Tree::default();
        t.anchor(vp("mem:///r/a"));
        t.insert_children(vp("mem:///r/a"), vec![vp("mem:///r/a/y")]);
        t.set_cursor(1);
        t.expand();
        t.insert_children(vp("mem:///r/a/y"), vec![vp("mem:///r/a/y/z")]);

        t.follow(&vp("mem:///r"));

        assert_eq!(t.root(), Some(&vp("mem:///r")));
        // The new root has not been listed yet, so this turn shows only it:
        // what matters is that NOTHING was discarded. `wants` requests its
        // listing — the same one `anchor` would have requested — and with
        // it everything comes back.
        assert_eq!(t.wants(), Some(vp("mem:///r")));
        t.insert_children(vp("mem:///r"), vec![vp("mem:///r/a")]);

        let rows: Vec<VPath> = t.rows().into_iter().map(|r| r.path).collect();
        assert_eq!(
            rows,
            vec![
                vp("mem:///r"),
                vp("mem:///r/a"),
                vp("mem:///r/a/y"),
                vp("mem:///r/a/y/z"),
            ],
            "the old root ends up unfolded and what was open under it stays open"
        );
        assert_eq!(
            t.selected(),
            Some(vp("mem:///r")),
            "and the cursor, at the top"
        );
    }

    /// **Switching between two sibling panels does not empty the tree.**
    ///
    /// It is `Tab` with the tree open, i.e. the program's most used key:
    /// re-anchoring at the destination closed the tree on EVERY keystroke.
    /// The root climbs to the common ancestor once and stays there, so the
    /// second turn no longer moves anything.
    #[test]
    fn following_a_sibling_rises_to_the_common_ancestor_once() {
        let mut t = Tree::default();
        t.anchor(vp("mem:///r/a"));
        t.insert_children(vp("mem:///r/a"), vec![vp("mem:///r/a/y")]);
        t.set_cursor(1);
        t.expand();
        t.insert_children(vp("mem:///r/a/y"), Vec::new());

        t.follow(&vp("mem:///r/b"));
        assert_eq!(
            t.root(),
            Some(&vp("mem:///r")),
            "climbs to the common ancestor"
        );
        // A listing of the new root — the same one `anchor` would have
        // requested anyway — and what was open comes back whole.
        t.insert_children(vp("mem:///r"), vec![vp("mem:///r/a"), vp("mem:///r/b")]);
        assert!(
            t.rows().iter().any(|f| f.path == vp("mem:///r/a/y")),
            "what was open is still there: {:?}",
            t.rows()
        );
        assert_eq!(
            t.selected(),
            Some(vp("mem:///r/b")),
            "and the cursor, on `b`"
        );

        // This turn no longer moves the root nor requests anything: `a`
        // hangs from `r`.
        t.follow(&vp("mem:///r/a"));
        assert_eq!(t.root(), Some(&vp("mem:///r")));
        assert_eq!(t.wants(), None, "no need for another trip");
        assert!(t.rows().iter().any(|f| f.path == vp("mem:///r/a/y")));
        assert_eq!(t.selected(), Some(vp("mem:///r/a")));
    }

    /// Another provider DOES anchor and empty out: there is no common
    /// ancestor, and another machine's branches say nothing about this
    /// one.
    #[test]
    fn following_to_another_provider_re_anchors() {
        let mut t = with_root();
        t.insert_children(vp("mem:///r"), vec![vp("mem:///r/a")]);
        t.follow(&vp("file:///otro/z"));
        assert_eq!(t.root(), Some(&vp("file:///otro/z")));
        assert_eq!(t.rows().len(), 1, "only the new root");
        assert_eq!(t.revealing(), None, "anchoring leaves nothing pending");
    }

    /// Following the SAME place TWICE does not move the cursor again: this
    /// funnel is where refreshes and clicks also pass through, and the
    /// cursor the reader moved by hand to look at another branch is
    /// theirs.
    #[test]
    fn following_twice_to_the_same_place_does_not_touch_the_cursor() {
        let mut t = with_root();
        t.insert_children(vp("mem:///r"), vec![vp("mem:///r/a"), vp("mem:///r/b")]);
        assert!(t.follow(&vp("mem:///r/a")), "the first time does move");
        assert_eq!(t.selected(), Some(vp("mem:///r/a")));

        t.set_cursor(2);
        assert!(!t.follow(&vp("mem:///r/a")), "the second moves nothing");
        assert_eq!(
            t.selected(),
            Some(vp("mem:///r/b")),
            "the cursor stays where the reader left it"
        );
    }

    /// And a target that will NOT arrive is released as soon as it is
    /// known: a branch's listing is capped, so the needed child can be
    /// left out and the target would hang forever.
    #[test]
    fn a_target_that_will_never_arrive_is_released() {
        let mut t = with_root();
        t.insert_children(vp("mem:///r"), vec![vp("mem:///r/a")]);
        t.follow(&vp("mem:///r/a/y"));
        assert_eq!(t.revealing(), Some(&vp("mem:///r/a/y")));

        // `a` gets listed and `y` is not in it: trimmed, deleted, or never
        // existed.
        t.insert_children(vp("mem:///r/a"), vec![vp("mem:///r/a/otro")]);
        assert_eq!(t.revealing(), None, "already known that row is not coming");
    }

    /// And a deep branch not yet listed is revealed IN INSTALLMENTS:
    /// `wants` requests one level per turn and the cursor lands once the
    /// row finally exists, not at the deepest ancestor there happened to
    /// be.
    #[test]
    fn following_an_unlisted_branch_waits_for_it_to_arrive() {
        let mut t = with_root();
        t.insert_children(vp("mem:///r"), vec![vp("mem:///r/a")]);

        t.follow(&vp("mem:///r/a/y"));
        assert_eq!(t.cursor(), 0, "no row to show yet");
        assert_eq!(t.revealing(), Some(&vp("mem:///r/a/y")));
        assert_eq!(t.wants(), Some(vp("mem:///r/a")), "`a` needs listing");

        t.insert_children(vp("mem:///r/a"), vec![vp("mem:///r/a/y")]);
        assert_eq!(t.selected(), Some(vp("mem:///r/a/y")));
        assert_eq!(t.revealing(), None);
    }

    /// Following the root itself changes nothing's place.
    #[test]
    fn following_to_its_own_root_drops_nothing() {
        let mut t = with_root();
        t.insert_children(vp("mem:///r"), vec![vp("mem:///r/a")]);
        t.set_cursor(1);
        t.expand();
        t.insert_children(vp("mem:///r/a"), vec![vp("mem:///r/a/y")]);

        t.follow(&vp("mem:///r"));

        assert_eq!(t.cursor(), 0);
        assert_eq!(t.rows().len(), 3, "`a` stays unfolded");
    }

    /// The cursor does not run off either end, and over an empty tree it
    /// selects nothing.
    #[test]
    fn the_cursor_stays_inside() {
        let mut t = Tree::default();
        t.down();
        assert_eq!(t.cursor(), 0);
        assert_eq!(t.selected(), None);
        let mut t = with_root();
        t.insert_children(vp("mem:///r"), vec![vp("mem:///r/a")]);
        t.down();
        t.down();
        t.down();
        assert_eq!(t.cursor(), 1);
        assert_eq!(t.selected(), Some(vp("mem:///r/a")));
        t.up();
        t.up();
        assert_eq!(t.cursor(), 0);
    }
}
