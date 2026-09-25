//! The branch tree in the side panel.
//!
//! Part of `controller`: these are methods of `State`, moved here without
//! touching them (ADR 0086). The only writer is still the actor.

// These modules are the same `impl State` split into pieces, so they use
// the same imports as the parent. Listing them here would be a forty-line
// list per file, across 32 files, that goes out of sync the moment the
// parent imports something — `super::*` keeps it in sync on its own.
#[allow(clippy::wildcard_imports)]
use super::*;

impl State {
    /// Cap on child branches of ONE branch.
    ///
    /// A directory with a hundred thousand subdirectories is not painted: it
    /// is clamped, and what is shown is "up to here". Without a cap, a single
    /// open branch turns every host snapshot into a message of megabytes.
    pub(super) const MAX_BRANCHES: usize = 2000;

    /// The tree's slot, if the layout places one.
    pub(super) fn branches_slot(&self) -> Option<SlotId> {
        self.tree
            .slot_ids()
            .into_iter()
            .find(|s| kind_de(&self.tree, *s).is_some_and(|k| k.as_str() == "tree"))
    }

    /// Anchors the tree wherever the focused listing is LOOKING.
    pub(super) fn seed_branches(&mut self) {
        // Near, not AT, the directory: hung right off it, a directory with no
        // subfolders was a one-row tree (captured 2026-09-21).
        let dir = self.slot().pane.dir().clone();
        self.branches
            .get_or_insert_with(norte_frontend::tree::Tree::default)
            .anchor_near(&dir, &norte_frontend::shell::home_vpath());
        self.gen_branches += 1;
    }

    /// The tree follows the ACTIVE listing: reveals its directory and
    /// requests whatever is missing to paint it.
    ///
    /// It is called from the funnel every listing that lands passes through
    /// ([`State::land_listing`]) and from the focus change, which are
    /// the two moments when "where the panel is looking" changes. Putting it
    /// in every gesture that triggers a `cd` — the mouse, the palette, the
    /// menu, the trail, the tree itself — would be the list that falls short
    /// one day.
    ///
    /// Only the ACTIVE one. A listing on the other side that finishes
    /// loading is not where the reader is working, and moving the tree for
    /// it would leave it pointing at a panel nobody is looking at.
    ///
    /// And it reveals, it does not re-anchor
    /// ([`norte_frontend::tree::Tree::follow`]): what the reader opened by
    /// hand stays open.
    pub(super) fn follow_branches(
        &mut self,
        slot: u32,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> bool {
        if self.branches_slot().is_none() || slot != self.active() {
            return false;
        }
        let Some(dir) = self.slots.get(&slot).map(|h| h.pane.dir().clone()) else {
            return false;
        };
        let moved = self
            .branches
            .get_or_insert_with(norte_frontend::tree::Tree::default)
            .follow(&dir);
        if !moved {
            // The listing has not moved — a refresh, a click — so neither
            // does the tree. Bumping the generation here would have
            // invalidated every painted index and turned a click in flight
            // on a branch into a generation rejection, with nothing having
            // changed.
            return false;
        }
        // The rows have moved — there are expanded ancestors that were not
        // there before — so every index painted until now names a different
        // branch.
        self.gen_branches += 1;
        self.request_branches(backend, mailbox);
        true
    }

    /// Requests the next branch that is needed, ONE per round.
    ///
    /// Lazy for the same reason the local listing does not bring sizes: a
    /// tree that read itself whole on opening would take minutes on a large
    /// `$HOME` and hours against a remote. One branch per round also bounds
    /// what a huge directory or a slow server can jam up: the next one
    /// requests the one after.
    pub(super) fn request_branches(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) {
        if self.branches_slot().is_none() {
            return;
        }
        let Some(dir) = self
            .branches
            .as_ref()
            .and_then(norte_frontend::tree::Tree::wants)
        else {
            return;
        };
        let backend = Arc::clone(backend);
        let mailbox = mailbox.clone();
        tokio::spawn(async move {
            // No attributes: the tree shows directory names and nothing
            // else, and requesting sizes or permissions per branch would be
            // paying for them on every folder someone expands.
            let children = match backend.list(dir.clone(), Vec::new()).await {
                Ok((stream, _)) => Some(Self::listing_branches(stream).await),
                // A branch that cannot be read: the shared model decides
                // (`Tree::branch_unreadable`).
                Err(_) => None,
            };
            let _ = mailbox
                .send(Message::Background(Box::new(Background::TreeBranches(
                    dir, children,
                ))))
                .await;
        });
    }

    /// The subdirectories of a listing, in the same order as the panel next
    /// to it.
    pub(super) async fn listing_branches(mut stream: norte_client::EntryStream) -> Vec<VPath> {
        use futures::StreamExt as _;
        let mut entries = Vec::new();
        while entries.len() < Self::MAX_BRANCHES {
            match stream.next().await {
                Some(Ok(e)) => {
                    if e.kind == norte_proto::EntryKind::Dir {
                        entries.push(e);
                    }
                }
                // An error mid-branch keeps what was read: half a branch
                // shows less than there is, but it never shows anything
                // FALSE, and the alternative is throwing away the work on a
                // huge directory for its last entry.
                Some(Err(_)) | None => break,
            }
        }
        // The SAME comparator as the listing next to it: two columns that
        // show the same thing in a different order read as if they said
        // different things.
        norte_frontend::sort_entries(&mut entries);
        entries.into_iter().map(|e| e.path).collect()
    }

    /// A branch's children arrived.
    ///
    /// And the next one is requested right here: that is what chains the
    /// lazy walk without a clock in between.
    pub(super) fn apply_branches(
        &mut self,
        dir: VPath,
        children: Option<Vec<VPath>>,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> Option<BridgeEnvelope<UiUpdate>> {
        // Only if the tree EXISTS in this layout: a late response for a panel
        // that is already closed does not resurrect its state nor send a
        // snapshot.
        self.branches_slot()?;
        let tree = self.branches.as_mut()?;
        match children {
            Some(h) => tree.insert_children(dir, h),
            None => tree.branch_unreadable(dir),
        }
        // The new rows are inserted IN THE MIDDLE: every index painted until
        // now names a different branch.
        self.gen_branches += 1;
        self.request_branches(backend, mailbox);
        let snap = self.snapshot();
        Some(self.over(UiUpdate::Snapshot(Box::new(snap))))
    }

    /// The tree, projected.
    ///
    /// With no state it projects EMPTY instead of not projecting at all: a
    /// slot the layout places and the host does not paint would vanish from
    /// the screen, and preserving what is there is the session's rule (ADR
    /// 0059).
    pub(super) fn branch_tree(&self, id: u32) -> crate::dto::TreeSlotView {
        let empty = norte_frontend::tree::Tree::default();
        let tree = self.branches.as_ref().unwrap_or(&empty);
        let rows_in = tree.rows();
        let root = tree.root().cloned();
        let rows = rows_in
            .iter()
            .map(|r| {
                // The root carries its whole path: a bare "`/`", or the name
                // of the last folder, do not say where this hangs from.
                let (displayable, hostile) = if root.as_ref() == Some(&r.path) {
                    norte_frontend::path_display(&r.path)
                } else {
                    // Without a name only a provider's root, and that one
                    // already went through the other branch: even so, the
                    // whole path is painted instead of staying blank.
                    r.path.file_name().map_or_else(
                        || norte_frontend::path_display(&r.path),
                        |n| norte_frontend::display_name(n.as_bytes()),
                    )
                };
                crate::dto::TreeRowView {
                    label: clamp_display(displayable),
                    hostile,
                    depth: u32::try_from(r.depth).unwrap_or(u32::MAX),
                    expanded: r.expanded,
                    children: r.children,
                }
            })
            .collect();
        crate::dto::TreeSlotView {
            slot_id: id,
            rows,
            cursor: tree.cursor() as u64,
            generation: self.gen_branches,
        }
    }

    /// A click on a branch: selects it, and depending on the gesture,
    /// navigates or collapses it.
    pub(super) fn touch_branch(
        &mut self,
        row: u32,
        generation: u64,
        navigate: bool,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if generation != self.gen_branches {
            // What was clicked and what is there now are not the same tree:
            // a branch's children land IN THE MIDDLE. Rejecting is the only
            // correct thing — going ahead would have navigated to a
            // different folder.
            return (Self::stale(StaleAction::Generation), Vec::new());
        }
        let Some(tree) = self.branches.as_mut() else {
            return (Self::stale(StaleAction::Modal), Vec::new());
        };
        if row as usize >= tree.rows().len() {
            return (Self::stale(StaleAction::Generation), Vec::new());
        }
        tree.set_cursor(row as usize);
        if !navigate {
            tree.toggle();
            self.gen_branches += 1;
            self.request_branches(backend, mailbox);
            let snap = self.snapshot();
            return (
                self.applied(),
                vec![self.over(UiUpdate::Snapshot(Box::new(snap)))],
            );
        }
        // Expand AND navigate: whoever clicks a branch wants to see what is
        // inside, and seeing it in the listing is the complete answer.
        tree.expand();
        let Some(destination) = tree.selected() else {
            return (Self::stale(StaleAction::Generation), Vec::new());
        };
        self.gen_branches += 1;
        self.request_branches(backend, mailbox);
        // To the FOCUSED listing, through the same path as any other
        // navigation: that is what makes having the tree open not change
        // where operations go.
        (
            self.applied(),
            self.navigate(&destination, Trail::Record, backend, mailbox),
        )
    }
}
