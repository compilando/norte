//! Composite core operations (spec §5): copy/move/delete over the
//! `Provider` contract. Providers do simple operations; HERE lives the
//! recursion, the collision and symlink policies (ADR 0005), the retries
//! with backoff, and the per-chunk cancellation check (rule 3).

use std::sync::Arc;

use futures::{FutureExt, StreamExt};
use norte_proto::SymlinkPolicy;
use norte_proto::{
    CollisionPolicy, ConflictKind, DeleteMode, Entry, EntryKind, Error, Segment, VPath,
    VerifyPolicy,
};
use norte_vfs::{Provider, SymlinkKind};
use tokio_util::sync::CancellationToken;

use norte_vfs::{FollowLinks, NodeId};

use crate::engine::TransferOptions;
use crate::observer::{Mutation, MutationObserver};
use crate::scheduler::TaskCtx;

/// The identity of what was just created at `path` (#369, ADR 0152).
///
/// For the places that create by PATH and have no confined root: `fs.mkdir`,
/// `fs.create`, `fs.write`, and packaging. There the path resolves —we just
/// published to it— so [`Dest::plain`]'s short loop is enough and inherits
/// its degradation: a failure is traced and noted with no identity.
///
/// These places having it is NOT a luxury. It is the same case as always
/// wearing another disguise: an agent writes a file, the human replaces it,
/// the human undoes the agent's session. With no identity, its replacement
/// goes to the trash.
pub(crate) async fn identity_of(
    provider: &dyn Provider,
    path: &VPath,
    observer: &Arc<dyn MutationObserver>,
) -> Option<norte_vfs::NodeId> {
    Dest::plain(provider, path.clone())
        .node_id_for(observer)
        .await
}

/// Maximum retries on transient errors (ADR 0005).
const MAX_RETRIES: u32 = 3;
/// Exponential backoff base: 100 ms · 2^n, deterministic (no jitter).
const BACKOFF_BASE_MS: u64 = 100;

/// WHERE an operation writes, and by which path.
///
/// The REAL path is always still there (`path`): it is the one that goes to
/// the journal, to the progress bar, and to error text, and the one read to
/// disambiguate a retry. What changes is HOW it is WRITTEN:
///
/// - with no confined root, against the provider and by absolute path, which
///   is the usual behavior;
/// - with one ([`norte_vfs::ConfinedRoot`]), by relative segments, and then
///   an intermediate component that is a symlink pointing outward cannot
///   redirect the write (#164, ADR 0054).
///
/// Built per DESTINATION —not per operation— because the relative one has to
/// correspond to the final path, and the collision policy may have renamed
/// it before anyone writes anything.
pub(crate) struct Dest<'a> {
    provider: &'a dyn Provider,
    path: VPath,
    /// The root and the relative path under it, when the destination knows
    /// how to confine itself.
    confined: Option<(&'a dyn norte_vfs::ConfinedRoot, Vec<Segment>)>,
}

impl<'a> Dest<'a> {
    /// An unconfined destination: absolute path against the provider.
    pub(crate) fn plain(provider: &'a dyn Provider, path: VPath) -> Self {
        Self {
            provider,
            path,
            confined: None,
        }
    }

    /// A destination under `root`, if there is one. `rel` are `path`'s
    /// segments hanging off the root; `None` in `root` leaves the
    /// destination unconfined, which is what a backend that does not know
    /// how gets.
    pub(crate) fn under(
        provider: &'a dyn Provider,
        root: Option<&'a dyn norte_vfs::ConfinedRoot>,
        rel: Vec<Segment>,
        path: VPath,
    ) -> Self {
        // An EMPTY relative is the root itself, and none of these operations
        // act on it: with no final segment there is no name to create, so it
        // degrades to the usual path instead of inventing one.
        let confined = root.filter(|_| !rel.is_empty()).map(|r| (r, rel));
        Self {
            provider,
            path,
            confined,
        }
    }

    /// The real path. For the journal, progress, errors, and reads.
    pub(crate) fn path(&self) -> &VPath {
        &self.path
    }

    /// The destination's provider. For what this handle does not cover —
    /// reading, disambiguating, `copy_native`—, which is not what needs
    /// confining.
    pub(crate) fn provider(&self) -> &'a dyn Provider {
        self.provider
    }

    async fn mkdir(&self) -> Result<(), Error> {
        match &self.confined {
            Some((root, rel)) => root.mkdir(rel).await,
            None => self.provider.mkdir(&self.path).await,
        }
    }

    async fn write(&self) -> Result<Box<dyn norte_vfs::ByteSink>, Error> {
        match &self.confined {
            Some((root, rel)) => root.write(rel).await,
            None => self.provider.write(&self.path).await,
        }
    }

    /// Like [`Provider::open_resumable`]. A confined destination does NOT
    /// resume: its staging is ephemeral, so it opens fresh and the caller
    /// re-copies the whole thing —see [`Dest::resumes`], which is what
    /// prevents a retry from leaving behind partials nobody is going to
    /// continue—.
    async fn open_resumable(&self) -> Result<(Box<dyn norte_vfs::ByteSink>, u64), Error> {
        match &self.confined {
            Some((root, rel)) => root.open_resumable(rel).await,
            None => self.provider.open_resumable(&self.path).await,
        }
    }

    /// Can this destination continue one of its own partials?
    ///
    /// The ROOT answers it when there is one (#297). It used to be `false`
    /// for every confined destination, because confined staging carried an
    /// ephemeral name per sink and keeping it would leave a
    /// `.norte-partial` per attempt that no later `open_resumable` was going
    /// to find. Since the local root knows how to open STABLE staging, that
    /// reason stopped applying — and keeping it turned `ResumePolicy::On`
    /// into a no-op precisely for the case where resuming matters most: a
    /// large, lone file.
    pub(crate) fn resumes(&self) -> bool {
        match &self.confined {
            Some((root, _)) => root.resumes(),
            None => true,
        }
    }

    async fn symlink(&self, target: &[u8], kind: SymlinkKind) -> Result<(), Error> {
        match &self.confined {
            Some((root, rel)) => root.symlink(rel, target, kind).await,
            None => self.provider.symlink(&self.path, target, kind).await,
        }
    }

    /// The destination's `lstat`, BY DESCRIPTOR when there is one (#218).
    ///
    /// Collision resolution used to look at the destination by path, so with
    /// a substituted intermediate component it would `lstat` a file from the
    /// attacker's tree — and that was the one `Overwrite` decided to delete.
    async fn stat(&self) -> Result<Entry, Error> {
        match &self.confined {
            Some((root, rel)) => root.stat(rel).await,
            None => self.provider.stat(&self.path).await,
        }
    }

    /// The identity of what is at the destination, by descriptor when there
    /// is one (#369).
    ///
    /// Asked right after publishing, so the journal points at WHAT it
    /// created and not just where it said it was creating it. By descriptor
    /// and not by path because the case this exists to cover is precisely
    /// the one where the path no longer leads here: with the destination
    /// folder renamed to the trash, `provider.node_id(/destination/f0001)`
    /// answers `NotFound` and the entry is left with no identity exactly
    /// when it needs it most.
    ///
    /// A failure does NOT propagate: identity is an improvement to reversal,
    /// not a requirement of the copy, and taking down a whole copy because
    /// an extra `lstat` did not come through would be trading a smarter undo
    /// for a more fragile copy. With no identity, undo behaves as it used to
    /// (ADR 0152).
    ///
    /// But it IS reported. Degrading silently is how this thing's first
    /// version spent an afternoon answering `None` with nothing squeaking,
    /// and there is one concrete error that deserves a shout and not a
    /// whisper: an `EscapesRoot` here means what was just published is not
    /// under the root we think it is — the same danger `copy_file`'s retry
    /// path treats as a security problem two screens further down.
    /// [`Self::node_id`], but without asking whether whoever is going to note
    /// the mutation is going to use it.
    ///
    /// It is the difference between a ten-thousand-file sync and the same
    /// one with ten thousand extra `stat`s: `sync.apply` copies with a no-op
    /// observer and notes on its own, so every identity this path found out
    /// would be thrown away. Against a remote destination, moreover, each
    /// one is a network trip.
    async fn node_id_for(&self, observer: &Arc<dyn MutationObserver>) -> Option<norte_vfs::NodeId> {
        if !observer.wants_identity() {
            return None;
        }
        self.node_id().await
    }

    async fn node_id(&self) -> Option<norte_vfs::NodeId> {
        let r = match &self.confined {
            // A root that does not know how to give an identity is not the
            // end of the road: the path can still resolve perfectly. It does
            // not happen today —the local root is the only one there is and
            // it does give one— but this way the next one does not silently
            // lose the guard.
            Some((root, rel)) => match root.node_id(rel).await {
                Ok(None) => {
                    self.provider
                        .node_id(&self.path, norte_vfs::FollowLinks::No)
                        .await
                }
                other => other,
            },
            None => {
                self.provider
                    .node_id(&self.path, norte_vfs::FollowLinks::No)
                    .await
            }
        };
        match r {
            Ok(id) => id,
            Err(
                e @ Error::Conflict {
                    conflict: ConflictKind::EscapesRoot,
                },
            ) => {
                tracing::warn!(
                    error = %e,
                    "what was just published does not resolve under the root: noted with no identity",
                );
                None
            }
            Err(e) => {
                tracing::debug!(
                    error = %e,
                    "no identity for the `created`: its undo will behave as before ADR 0152",
                );
                None
            }
        }
    }

    /// Deletes the destination's LEAF, by descriptor when there is one (#218).
    ///
    /// No fallback to the by-path route: a root that says it does not know
    /// how to delete confined returns [`Error::Unsupported`] and the caller
    /// REJECTS the policy. Falling back to the path would reopen the hole
    /// exactly in the case this method exists to close, and silently on top
    /// of it.
    async fn remove(&self, cancel: &CancellationToken) -> Result<(), Error> {
        match &self.confined {
            // The SAME loop as the by-path route, and not a bare
            // `with_retry`: without it, an `unlinkat` that suffers a
            // transient and then answers `ENOENT` comes out as a hard
            // `NotFound` and kills `Overwrite`, instead of counting as done
            // — which is exactly the hole #186 documented through the other
            // door.
            Some((root, rel)) => delete_loop(|| root.remove(rel), cancel)
                .await
                .map_err(|(e, _)| e),
            None => remove_retrying(self.provider, &self.path, cancel).await,
        }
    }

    /// This destination's partial digest, by descriptor when there is one.
    async fn partial_digest(&self, len: u64) -> Result<Option<[u8; 32]>, Error> {
        match &self.confined {
            Some((root, rel)) => root.partial_digest(rel, len).await,
            None => self.provider.partial_digest(&self.path, len).await,
        }
    }

    /// Deletes this destination STATING its class, and counting the
    /// ambiguity (#296).
    ///
    /// A post-order delete reaches directories and leaves, and `unlinkat`
    /// needs to know which one: they are two distinct effects, and confusing
    /// them is like deleting a tree while believing a file is being deleted.
    /// By path the distinction is not needed —`Provider::remove` does it
    /// internally— so that branch is the usual one.
    pub(crate) async fn remove_kind(
        &self,
        is_dir: bool,
        cancel: &CancellationToken,
    ) -> Result<(), (Error, Ambiguity)> {
        match &self.confined {
            Some((root, rel)) => {
                if is_dir {
                    delete_loop(|| root.rmdir(rel), cancel).await
                } else {
                    delete_loop(|| root.remove(rel), cancel).await
                }
            }
            None => remove_retrying_amb(self.provider, &self.path, cancel).await,
        }
    }
}

/// The destination of a RECURSIVE operation: its provider, its confined root
/// if there is one, and the base path everything it is going to write hangs
/// from.
///
/// Kept together because all three are needed together at every step, and
/// because this way the root is opened ONCE per operation instead of once per
/// leaf (#164).
pub(crate) struct Destination<'a> {
    provider: &'a dyn Provider,
    root: Option<&'a dyn norte_vfs::ConfinedRoot>,
    base: &'a VPath,
}

impl<'a> Destination<'a> {
    /// With the root [`open_dest_root`] managed to open.
    pub(crate) fn new(
        provider: &'a dyn Provider,
        root: Option<&'a dyn norte_vfs::ConfinedRoot>,
        base: &'a VPath,
    ) -> Self {
        Self {
            provider,
            root,
            base,
        }
    }

    /// No root: the destination does not know how to confine itself, or
    /// there is no directory to hang from. Every path goes as is, which is
    /// the usual thing.
    pub(crate) fn unconfined(provider: &'a dyn Provider, base: &'a VPath) -> Self {
        Self::new(provider, None, base)
    }

    /// The provider, for what is not confined (reading, disambiguating,
    /// collisions).
    pub(crate) fn provider(&self) -> &'a dyn Provider {
        self.provider
    }

    /// The destination of ONE path under this base, confined if it falls
    /// inside it.
    pub(crate) fn at(&self, path: VPath) -> Dest<'a> {
        match self.root.zip(rel_under(self.base, &path)) {
            Some((root, rel)) => Dest::under(self.provider, Some(root), rel, path),
            None => Dest::plain(self.provider, path),
        }
    }
}

/// Is the root that was opened the node currently at `path` RIGHT NOW?
///
/// Confinement anchors on a descriptor, but the descriptor is obtained by
/// opening a path, and that opening resolves symlinks like any other —on
/// purpose: a `~/backups -> /mnt/disk/backups` is a legitimate destination—.
/// Between creating the directory and opening it there is room for a swap
/// (`rmdir` + symlink), and then everything that comes after is perfectly
/// confined to the WRONG TREE, with nothing squeaking: this phase's security
/// review found it.
///
/// What closes it is comparing identities: the `stat` is an `lstat`, so a
/// `path` replaced by a symlink comes out with the LINK's identity and not
/// the opened directory's, and they do not match. Restoring the real
/// directory does not slip through either: it would be a different inode.
///
/// A backend with no stable identity (`Ok(None)` on either side) has nothing
/// to compare and passes: it is the same honest degradation as always, and
/// `node_id` already documents it.
async fn same_root_or_fail(
    dst: &dyn Provider,
    root: &dyn norte_vfs::ConfinedRoot,
    path: &VPath,
    cancel: &CancellationToken,
) -> Result<(), Error> {
    let opened = root.root_id().await?;
    let at_path = with_retry(cancel, || dst.node_id(path, FollowLinks::No).boxed()).await?;
    let (Some(opened), Some(at_path)) = (opened, at_path) else {
        return Ok(());
    };
    if opened == at_path {
        return Ok(());
    }
    tracing::error!(
        dest = %crate::engine::span_path(path),
        "the destination root changed between creating it and opening it: stopping instead of \
         writing confined into a different tree (#164)"
    );
    Err(Error::Conflict {
        conflict: ConflictKind::EscapesRoot,
    })
}

/// `path`'s segments hanging off `root`, or `None` if `path` is not under
/// `root` — in which case there is no relative to give and whoever asks is
/// left unconfined, which is the honest thing.
pub(crate) fn rel_under(root: &VPath, path: &VPath) -> Option<Vec<Segment>> {
    if path.scheme() != root.scheme() || path.authority() != root.authority() {
        return None;
    }
    let prefix: Vec<&[u8]> = root.segments().collect();
    let full: Vec<&[u8]> = path.segments().collect();
    if full.len() < prefix.len() || full[..prefix.len()] != prefix[..] {
        return None;
    }
    full[prefix.len()..]
        .iter()
        .map(|s| Segment::new(s.to_vec()).ok())
        .collect()
}

/// The same anchor check as [`open_leaf_root`], but by PATH and on `to`'s
/// PARENT (#295), for the paths that CREATE their destination.
///
/// A tree is copied by creating `to` and confining underneath, so at the
/// moment of checking there is no descriptor to ask: what the human listed is
/// the directory where the tree is going to land, i.e. the parent. The check
/// is by path and so has its own window —tiny, between asking and
/// creating—, but the case this anchor exists to close is the link **already
/// planted** before anyone looked, and it catches that just the same.
async fn anchor_parent_or_fail(
    dst: &dyn Provider,
    to: &VPath,
    anchor: Option<&norte_proto::DirAnchor>,
    task_id: u64,
    cancel: &CancellationToken,
) -> Result<(), Error> {
    let (Some(anchor), Some(dir)) = (anchor, to.parent()) else {
        return Ok(());
    };
    let observed = with_retry(cancel, || {
        dst.node_id(&dir, norte_vfs::FollowLinks::Yes).boxed()
    })
    .await?;
    match observed {
        Some(id) if crate::anchor::home(anchor, id) => Ok(()),
        Some(_) => {
            tracing::warn!(
                task_id,
                dest = %crate::engine::span_path(&dir),
                "the destination directory is no longer the node the client listed: refusing \
                 to write (#295)"
            );
            Err(Error::Conflict {
                conflict: norte_proto::ConflictKind::EscapesRoot,
            })
        }
        None => {
            tracing::debug!(
                task_id,
                dest = %crate::engine::span_path(&dir),
                "an anchor came in and this destination does not know how to give an identity: not checked (#295)"
            );
            Ok(())
        }
    }
}

/// A leaf's destination, with a root if `open_leaf_root` could give one.
fn leaf_destination<'a>(
    dst: &'a dyn Provider,
    dir: Option<&'a VPath>,
    root: Option<&'a dyn norte_vfs::ConfinedRoot>,
    to: &'a VPath,
) -> Destination<'a> {
    match dir {
        Some(dir) => Destination::new(dst, root, dir),
        None => Destination::unconfined(dst, to),
    }
}

/// The root for transferring ONE leaf: its destination DIRECTORY (#219).
///
/// Returns `(directory, root)`; `None` in the root = there is nowhere to
/// anchor to and the caller continues by path, which is the usual thing.
///
/// # Why the directory and not the policy scope
///
/// The issue proposed the scope. It does not work, and the reason is
/// simple: **a human `fs.copy` has no scope** — scopes exist only for agent
/// sessions, so for the most common caller there would be nothing to open.
///
/// What the human DID approve is the destination directory: it is what the
/// pane showed, what `request_transfer` composes `to` from, and what the
/// dialog names in its own field.
///
/// # What it closes, and what it does not
///
/// It closes the SWAP, which is #164's model: between looking at the
/// directory and writing to it, someone replaces it with a link to another
/// tree. The identity check catches it —the opened node stops being the one
/// that was looked at— and, once opened, staging and its publication both go
/// through the descriptor: `dest/sub` is resolved ONCE instead of three times
/// —creating the staging, renaming, and once more for each of the three
/// 100/200/400 ms retries.
///
/// A link that ALREADY was there when the core first looked is not closed by
/// that confinement, and it cannot be: from here a legitimate
/// `~/backups -> /mnt/disk/backups` and a hostile link are indistinguishable,
/// because both resolve elsewhere. What separates them is the identity
/// observed when APPROVING —the listing the human looked at—, and since #295
/// that identity TRAVELS with the request: it is this function's `anchor`
/// (ADR 0073). With no anchor, a destination directory that is a link is
/// copied by path, exactly as before #219, and it is stated in the log.
///
/// What is still not closed is the substitution of an INTERMEDIATE component
/// of the directory: the root is obtained by OPENING a path, so that first
/// resolution is by path by definition, and the anchor names the final
/// directory, not the ones above it. It is the same residue a recursive copy
/// accepts for its own destination.
///
async fn open_leaf_root(
    dst: &dyn Provider,
    to: &VPath,
    anchor: Option<&norte_proto::DirAnchor>,
    task_id: u64,
    cancel: &CancellationToken,
) -> Result<(Option<VPath>, Option<Box<dyn norte_vfs::ConfinedRoot>>), Error> {
    // A leaf whose destination is a ROOT has no directory to hang from.
    let Some(dir) = to.parent() else {
        return Ok((None, None));
    };
    // Is the destination directory a LINK? Asked with the same path's two
    // resolutions: if `lstat` and `stat` give the same node, the last
    // component is a real directory; if they differ, it is a link.
    //
    // And if it is, it is NOT confined. It is not a concession: it is that
    // there is nothing to check there. `~/backups -> /mnt/disk/backups` is a
    // legitimate, ordinary destination —on macOS so are `/tmp`, `/var`, and
    // `/etc`; on a usrmerge Linux, `/bin` and `/lib`—, and a freshly planted
    // hostile link looks EXACTLY the same from here: both resolve elsewhere.
    // Rejecting both would break the copy for half the distributions to
    // close nothing; accepting both with the check off is the usual thing,
    // which is what is done, stating it.
    let (via_link, direct_id) = (
        with_retry(cancel, || dst.node_id(&dir, FollowLinks::Yes).boxed()).await?,
        with_retry(cancel, || dst.node_id(&dir, FollowLinks::No).boxed()).await?,
    );
    let is_link = matches!((via_link, direct_id), (Some(a), Some(b)) if a != b);
    let root = match dst.open_root(&dir).await {
        Ok(r) => Some(r),
        // "I don't know how to confine": the only degrading case, same as in
        // a recursive copy and for the same reason (ADR 0054).
        Err(Error::Unsupported) => {
            // `debug!` and not `warn!`, the opposite of a recursive copy:
            // that one warns once per OPERATION and this one once per FILE,
            // and the window's batch queues a Task per mark. Five thousand
            // copies to an SFTP, a bucket, or from Windows —where
            // `open_root` is the default `Unsupported`— would write five
            // thousand identical lines, and a warning repeated five thousand
            // times is not a warning.
            tracing::debug!(
                task_id,
                dest = %crate::engine::span_path(&dir),
                "the destination does not know how to confine its writes: an intermediate \
                 symlink could redirect this transfer outside its directory (#219)"
            );
            None
        }
        // The destination directory is not there. It is the answer, not a
        // confinement failure: the write was going to fail anyway, and
        // stating it here avoids the alarmist message from the arm below.
        Err(e @ Error::NotFound) => return Err(e),
        Err(e) => {
            tracing::error!(
                task_id,
                dest = %crate::engine::span_path(&dir),
                error = %e,
                "could not open the confined destination directory and the destination declared \
                 that it knew how: stopping instead of writing by path without saying so (#219)"
            );
            return Err(e);
        }
    };
    // The identity check ONLY if the last component is a real directory. On
    // a link it compares the LINK's node with the one it points to, which
    // never match: it would reject a legitimate
    // `~/backups -> /mnt/disk/backups`, and on macOS a `/tmp`.
    //
    // It is confined just the same, without it: the descriptor still holds
    // the value it holds —one resolution instead of three plus the
    // retries— and what is lost is only the check, which on a link could
    // not say anything.
    if let Some(root) = root.as_deref()
        && !is_link
    {
        same_root_or_fail(dst, root, &dir, cancel).await?;
    }
    // The ANCHOR (#295, ADR 0073): is this directory still the node the
    // human was looking at when approving?
    //
    // It is the ONLY thing separating a `dest/sub -> /etc` planted before
    // anyone looked from a legitimate `~/backups -> /mnt/disk/backups`,
    // because both resolve elsewhere and look the same from here. And it is
    // answered against the ALREADY opened DESCRIPTOR —`root_id`—, without
    // resolving the path again: the root being checked is exactly the root
    // being written through, with no window in between. On a link it holds
    // just the same, which is the difference from the check above: the
    // anchor does not care by which name it was reached, only WHICH NODE.
    if let Some(anchor) = anchor {
        let observed = match root.as_deref() {
            Some(root) => root.root_id().await?,
            // With no confined root (the destination does not know how)
            // what is left is the path, which is worse and is stated: it
            // still closes the link that was ALREADY planted, which is the
            // issue's case, and not the substitution afterward.
            None => {
                with_retry(cancel, || {
                    dst.node_id(&dir, norte_vfs::FollowLinks::Yes).boxed()
                })
                .await?
            }
        };
        match observed {
            Some(id) if crate::anchor::home(anchor, id) => {}
            Some(_) => {
                tracing::warn!(
                    task_id,
                    dest = %crate::engine::span_path(&dir),
                    "the destination directory is no longer the node the client listed: refusing \
                     to write (#295)"
                );
                return Err(Error::Conflict {
                    conflict: norte_proto::ConflictKind::EscapesRoot,
                });
            }
            // The destination does not know how to give a node identity. It
            // cannot be checked and NO verdict is invented: it continues, as
            // without an anchor. A client against a destination like that
            // did not receive an anchor to send either, so reaching here
            // means a client made one up or a destination changed its mind.
            None => tracing::debug!(
                task_id,
                dest = %crate::engine::span_path(&dir),
                "an anchor came in and this destination does not know how to give an identity: not checked (#295)"
            ),
        }
    }
    if is_link {
        tracing::debug!(
            task_id,
            dest = %crate::engine::span_path(&dir),
            "the destination directory is a link: confined, but its identity cannot be \
             checked (#219)"
        );
    }
    Ok((Some(dir), root))
}

/// Opens `root`'s confined root, or says why there is none.
///
/// **A destination that does not know how to confine itself is not
/// rejected: a warning is given and it continues.** The opposite would leave
/// an SFTP, a bucket, or a Windows uncopyable to over a defense those
/// destinations cannot give, and the hole it closes needs someone to plant a
/// symlink at just the right moment. The warning fires ONCE per operation,
/// not once per step.
pub(crate) async fn open_dest_root(
    dst: &dyn Provider,
    root: &VPath,
    task_id: u64,
) -> Result<Option<Box<dyn norte_vfs::ConfinedRoot>>, Error> {
    match dst.open_root(root).await {
        Ok(r) => Ok(Some(r)),
        // "I don't know how to confine": the only case where it degrades,
        // and it is the one ADR 0054 reasons about. The backend cannot, and
        // not copying to it because of that would be worse.
        Err(Error::Unsupported) => {
            tracing::warn!(
                task_id,
                dest = %crate::engine::span_path(root),
                "the destination does not know how to confine its writes: an intermediate \
                 symlink could redirect this operation outside its root (#164)"
            );
            Ok(None)
        }
        // **Any other failure STOPS the operation, and this is a change from
        // this phase's security review.** Degrading here used to be the
        // behavior, yes, but back then there was no promise to break: today
        // `capabilities_at` already told the human this destination confines
        // —the dialog told them by STAYING SILENT about it— and continuing
        // by path without saying so turns "make the `open` fail once" into
        // the key that reopens #164 for the whole operation. A momentary-swap
        // `ENOTDIR`, an `EMFILE`, a transient `EACCES`: all of them counted.
        //
        // The destination that CANNOT confine is already covered by the arm
        // above, so what falls here is only whoever said it could and failed.
        Err(e) => {
            tracing::error!(
                task_id,
                dest = %crate::engine::span_path(root),
                error = %e,
                "could not open the confined destination root and the destination declared \
                 that it knew how: stopping instead of writing by path without saying so (#164)"
            );
            Err(e)
        }
    }
}

/// Does it deserve a retry? Only what is explicitly transient; every other
/// error is NEVER retried (repeating a `Conflict` does not fix it).
fn is_transient(e: &Error) -> bool {
    matches!(
        e,
        Error::ProviderUnavailable { retryable: true } | Error::Io { retryable: true }
    )
}

/// Waits out attempt `attempt`'s backoff, cancelable DURING the wait (rule 3:
/// cancellation never waits on the backoff).
async fn backoff_or_cancel(cancel: &CancellationToken, attempt: u32) -> Result<(), Error> {
    let delay = std::time::Duration::from_millis(BACKOFF_BASE_MS << attempt);
    tokio::select! {
        () = cancel.cancelled() => Err(Error::Cancelled),
        () = tokio::time::sleep(delay) => Ok(()),
    }
}

/// Retries an IDEMPOTENT one-off operation (`stat`/`read`/`read_link`/
/// `node_id`) on transient errors: up to [`MAX_RETRIES`] retries with
/// cancelable exponential backoff.
///
/// MUTATIONS do not go through here: after a transient failure their effect
/// may have already applied (a post-commit timeout on remotes) and blindly
/// retrying them would duplicate effects or lie to the journal — they use
/// the `*_retrying` wrappers with per-operation disambiguation (issue #17).
/// The ambiguous write's COMMIT is also disambiguated (#32.1, in
/// [`copy_file`]: presence + size of the destination). Remaining debt (#32):
/// the ambiguous mkdir's `Created` (requires a pre-stat, +1 stat/dir — only
/// leaves one extra empty dir, never a loss) and `trash`'s retry (an OS op;
/// the journal's `dest` would be lost on retry).
pub(crate) async fn with_retry<'a, T: 'a>(
    cancel: &CancellationToken,
    mut op: impl FnMut() -> futures::future::BoxFuture<'a, Result<T, Error>>,
) -> Result<T, Error> {
    let mut attempt = 0u32;
    loop {
        match op().await {
            Err(e) if attempt < MAX_RETRIES && is_transient(&e) => {
                backoff_or_cancel(cancel, attempt).await?;
                attempt += 1;
            }
            other => return other,
        }
    }
}

/// What is known about the EFFECT of a mutation that failed.
///
/// A one-off mutation that fails after a transient leaves the other end in
/// doubt: the `remove` may have arrived and the answer got lost (a remote's
/// post-commit timeout, issue #17). The `*_retrying` wrappers here have
/// always carried that doubt; what they did not do was COUNT it, and
/// whoever needs it is the caller deciding whether to journal.
///
/// The whole tree resolves the doubt toward "we did it" —that is already
/// what the `Err(NotFound) if ambiguous` arms further down and
/// `mkdir_retrying`'s re-list do— because the error of that choice is one
/// extra row, and the opposite one is an effect with no row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ambiguity {
    /// The failure arrived before anything changed: the effect was NOT applied.
    NotApplied,
    /// A transient got in the way: the effect MAY have been applied.
    MaybeApplied,
}

/// `remove` with retries and disambiguation (issue #17): after a transient
/// failure the effect may have applied — `NotFound` on retry means "it's
/// gone already", which IS the state the remove was after (whether our
/// first attempt deleted it or not, the journal records a single true
/// `Removed`).
pub(crate) async fn remove_retrying(
    p: &dyn Provider,
    path: &VPath,
    cancel: &CancellationToken,
) -> Result<(), Error> {
    remove_retrying_amb(p, path, cancel)
        .await
        .map_err(|(e, _)| e)
}

/// Like [`remove_retrying`], STATING whether the effect was left in doubt.
///
/// Requested by `sync::exec`'s tree deletion (#186): a `remove` that fails
/// after a transient may have reached the bucket, and if it was the first
/// node of the post-order, counting it as "not removed" leaves the tree
/// jagged and with no journal row — which is exactly the hole #186 closed
/// through the other door.
pub(crate) async fn remove_retrying_amb(
    p: &dyn Provider,
    path: &VPath,
    cancel: &CancellationToken,
) -> Result<(), (Error, Ambiguity)> {
    delete_loop(|| p.remove(path), cancel).await
}

/// [`remove_retrying_amb`]'s loop, over ANY way of deleting.
///
/// Exists because since #218 there are two: by path and by a confined
/// root's descriptor. Both need the same thing —checking cancellation before
/// the first attempt, sowing doubt on seeing a transient, and treating a
/// later `NotFound` as "it's gone already", which IS the state the delete
/// was after— and having it twice is having it two different ways the
/// moment one of the two is touched.
async fn delete_loop<F, Fut>(
    mut delete: F,
    cancel: &CancellationToken,
) -> Result<(), (Error, Ambiguity)>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<(), Error>>,
{
    let mut attempt = 0u32;
    let mut ambiguous = false;
    // The doubt, once sown, travels with EVERY error exit — including the
    // cancellation that cuts off the backoff, which is the one #186 managed
    // to produce live (the user sees the stall and hits Ctrl+K).
    let doubtful = |ambiguous: bool| {
        if ambiguous {
            Ambiguity::MaybeApplied
        } else {
            Ambiguity::NotApplied
        }
    };
    loop {
        if cancel.is_cancelled() {
            return Err((Error::Cancelled, doubtful(ambiguous)));
        }
        match delete().await {
            Ok(()) => return Ok(()),
            Err(Error::NotFound) if ambiguous => return Ok(()),
            // The doubt is sown the moment a transient is SEEN, not only
            // when a retry is decided: the effect may have ended up applied
            // on the other end regardless of what we do afterward, and
            // exhausting the retries and cancelling are exactly the two
            // exits through which #186 used to escape.
            Err(e) if is_transient(&e) => {
                ambiguous = true;
                if attempt >= MAX_RETRIES || cancel.is_cancelled() {
                    return Err((e, Ambiguity::MaybeApplied));
                }
                backoff_or_cancel(cancel, attempt)
                    .await
                    .map_err(|e| (e, Ambiguity::MaybeApplied))?;
                attempt += 1;
            }
            Err(e) => return Err((e, doubtful(ambiguous))),
        }
    }
}

/// `trash` with retries and disambiguation (#99). After a transient failure
/// the effect may have applied. A trash that NAMES its destination —the
/// logic, and `norte-vfs-local`'s freedesktop one since task 11b— recovers
/// the payload on retry with the SAME deterministic id (`Some`, keeps the
/// undo's `reversal_ref`). One that does not name it (macOS, Windows)
/// cannot: a `NotFound` on retry means "it's gone already" (our first
/// attempt trashed it) → `Ok(None)`, and undo degrades. A `NotFound` with NO
/// prior transient is the victim that never existed: it propagates.
pub(crate) async fn trash_retrying(
    provider: &dyn Provider,
    path: &VPath,
    id: &norte_vfs::trash::TrashId,
    cancel: &CancellationToken,
) -> Result<Option<VPath>, Error> {
    trash_retrying_amb(provider, path, id, cancel)
        .await
        .map_err(|(e, _)| e)
}

/// Like [`trash_retrying`], STATING whether the effect was left in doubt.
/// Same reason as [`remove_retrying_amb`] (#186).
pub(crate) async fn trash_retrying_amb(
    provider: &dyn Provider,
    path: &VPath,
    id: &norte_vfs::trash::TrashId,
    cancel: &CancellationToken,
) -> Result<Option<VPath>, (Error, Ambiguity)> {
    let mut attempt = 0u32;
    let mut ambiguous = false;
    let doubtful = |ambiguous: bool| {
        if ambiguous {
            Ambiguity::MaybeApplied
        } else {
            Ambiguity::NotApplied
        }
    };
    loop {
        if cancel.is_cancelled() {
            return Err((Error::Cancelled, doubtful(ambiguous)));
        }
        match provider.trash(path, id).await {
            Ok(dest) => return Ok(dest),
            Err(Error::NotFound) if ambiguous => return Ok(None),
            // The doubt is sown the moment a transient is SEEN, not only
            // when a retry is decided: the effect may have ended up applied
            // on the other end regardless of what we do afterward, and
            // exhausting the retries and cancelling are exactly the two
            // exits through which #186 used to escape.
            Err(e) if is_transient(&e) => {
                ambiguous = true;
                if attempt >= MAX_RETRIES || cancel.is_cancelled() {
                    return Err((e, Ambiguity::MaybeApplied));
                }
                backoff_or_cancel(cancel, attempt)
                    .await
                    .map_err(|e| (e, Ambiguity::MaybeApplied))?;
                attempt += 1;
            }
            Err(e) => return Err((e, doubtful(ambiguous))),
        }
    }
}

/// `mkdir` with retries and disambiguation (#32.2). CONTRACT: the caller
/// already verified the destination did NOT preexist ([`ensure_dir`]'s
/// pre-stat) — with that guarantee, a `Conflict` after a transient failure
/// is a CANDIDATE for our first attempt and is VERIFIED by listing it (#104
/// review MAJOR-1, the same criterion as `symlink_retrying`): a dir we just
/// created cannot have content — empty = ours (`Ok`, with its `Created` for
/// the journal); with content = someone else's, fail-safe `Conflict`.
/// Without the verification, a falsely claimed `Created` would make undo
/// (M3-2 routes `Created` through TRASH, no longer `remove`) trash the
/// FOREIGN dir with its content. Honest residual window: a third party that
/// creates the dir and has NOT yet put anything in it passes as ours
/// (indistinguishable without a node-id); its undo trashes a foreign EMPTY
/// dir — recoverable and bounded. A `Conflict` with NO prior transient IS a
/// real collision (an external race): it propagates without listing and the
/// caller's policy decides.
pub(crate) async fn mkdir_retrying(
    dest: &Dest<'_>,
    cancel: &CancellationToken,
) -> Result<(), Error> {
    let mut attempt = 0u32;
    let mut ambiguous = false;
    let (p, path) = (dest.provider(), dest.path());
    loop {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        match dest.mkdir().await {
            // An escape is a VERDICT, not a "maybe applied". It has to exit
            // before disambiguation: that reads by PATH, following the same
            // hostile component that just produced it, and an empty
            // directory on the other side would be read as "we created it"
            // — with its `Created` in the journal and an undo that trashes
            // something from outside the tree.
            Err(
                e @ Error::Conflict {
                    conflict: ConflictKind::EscapesRoot,
                },
            ) => return Err(e),
            Err(e @ Error::Conflict { .. }) if ambiguous => {
                // Disambiguation READS, and reading is not what this
                // destination is confined for: the `list` goes by path, as
                // always.
                let mut stream = with_retry(cancel, || p.list(path).boxed()).await?;
                return match stream.next().await {
                    None => Ok(()),
                    Some(Ok(_)) => Err(e),
                    Some(Err(le)) => Err(le),
                };
            }
            Err(e) if attempt < MAX_RETRIES && is_transient(&e) && !cancel.is_cancelled() => {
                ambiguous = true;
                backoff_or_cancel(cancel, attempt).await?;
                attempt += 1;
            }
            other => return other,
        }
    }
}

/// `symlink` with retries and disambiguation (issue #17): a `Conflict` after
/// a transient failure is verified by reading the link — if its target is
/// EXACTLY our bytes, it is our first attempt and counts as success (a
/// single `Created` for the journal). A different target = a real collision.
///
/// Documented limits: (a) a provider that CANONICALIZES the target on
/// re-reading it (Windows reconstructs from the reparse buffer; exotic SFTP
/// servers) would give a false negative → fail-safe `Conflict` with the
/// effect applied and a `Created` lost for the journal (same debt as mkdir,
/// #32); (b) the kind is not verified — a preexisting link with the SAME
/// target and a different kind would pass as ours (requires a transient +
/// exact preexistence; the target rules, there is never a loss).
pub(crate) async fn symlink_retrying(
    dest: &Dest<'_>,
    target: &[u8],
    kind: SymlinkKind,
    cancel: &CancellationToken,
) -> Result<(), Error> {
    let mut attempt = 0u32;
    let mut ambiguous = false;
    let (backend, link) = (dest.provider(), dest.path());
    loop {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        match dest.symlink(target, kind).await {
            Ok(()) => return Ok(()),
            // Terminal, for the same reason as in `mkdir_retrying`: the
            // disambiguation's `read_link` goes by path and could answer
            // that the link "is already ours" by reading one that is
            // outside the root.
            Err(
                e @ Error::Conflict {
                    conflict: ConflictKind::EscapesRoot,
                },
            ) => return Err(e),
            Err(e @ Error::Conflict { .. }) if ambiguous => {
                return match with_retry(cancel, || backend.read_link(link).boxed()).await {
                    Ok(bytes) if bytes == target => Ok(()),
                    Err(Error::Cancelled) => Err(Error::Cancelled),
                    _ => Err(e),
                };
            }
            Err(e) if attempt < MAX_RETRIES && is_transient(&e) && !cancel.is_cancelled() => {
                ambiguous = true;
                backoff_or_cancel(cancel, attempt).await?;
                attempt += 1;
            }
            Err(e) => return Err(e),
        }
    }
}

/// Is `to` the SAME node as `from`, i.e. the same file with a different
/// spelling? (#274)
///
/// Identity **and** spelling, and both are needed.
///
/// Identity alone is not enough: `NodeId` is `(device, inode)`, so two
/// DIFFERENT directory entries that are hardlinks to the same file give the
/// same id — and `mv a.txt b.txt` with `b.txt` linked to `a.txt` is not a
/// spelling change, it is a real collision with its own policy to apply.
/// `rename_applied` already documents that same hole a few lines below.
///
/// Spelling alone is not enough either: it compares folded keys, which is a
/// heuristic over NAMES, and what is decided here moves one file over
/// another.
///
/// So: same directory, leaves that fold to the same key under the mode that
/// directory uses, and the same node. The cheap check goes first, which is
/// what avoids one extra `node_id` on every collision of a massive `move`
/// against an sftp.
///
/// `from_id` comes captured BEFORE the first attempt, as #17 mandates.
async fn is_the_same_leaf_with_a_different_spelling(
    src: &dyn Provider,
    from: &VPath,
    to: &VPath,
    from_id: Option<norte_vfs::NodeId>,
    cancel: &CancellationToken,
) -> bool {
    if from == to || from.parent() != to.parent() {
        return false;
    }
    let (Some(a), Some(leaf_from), Some(leaf_to)) = (from_id, from.file_name(), to.file_name())
    else {
        return false;
    };
    let mode = fold_mode_at(to, src).await;
    if mode == norte_encoding::FoldMode::None
        || norte_encoding::name_key(leaf_from.as_bytes(), mode)
            != norte_encoding::name_key(leaf_to.as_bytes(), mode)
    {
        return false;
    }
    matches!(
        with_retry(cancel, || src.node_id(to, FollowLinks::No).boxed()).await,
        Ok(Some(b)) if a == b
    )
}

/// Changes a name's SPELLING: `Foo.txt → foo.txt` on a folding volume, where
/// the two names are the same node (#274).
///
/// In two steps and through an intermediate name, because the only thing
/// this provider knows how to do is rename WITHOUT OVERWRITING and the
/// destination "exists" —it is the source—: first to a name that collides
/// with nobody, and from there to the one requested, which by then is
/// already free. It is what anyone renaming `README` to `readme` on a Mac
/// does.
///
/// The intermediate name goes by PREFIX and does not embed the leaf
/// (`.norte-rename-case-<n>`), like the batch executor's
/// ([`crate::rename::naming`]): a suffix on a 250-byte leaf blows the
/// component's 255 limit —the corpus's `name_max_255` fixture exists for
/// that— and would also change the file's extension while it lasts. `n`
/// climbs until it finds a free one, because a real file can be named that.
///
/// # The window between the two steps is NOT cancelable, on purpose
///
/// It is the same decision `rename::exec` made: cancellation is checked
/// BETWEEN operations, never inside one. With the task's token, cancelling
/// between step 1 and step 2 made step 2 **and the way back** exit
/// immediately without trying anything, leaving the file with a name nobody
/// wrote and answering `Cancelled` — and in this repository a cancelled task
/// means "the tree is as it was".
///
/// If the second step fails, it goes back to the starting name. If even that
/// cannot be done, the error goes to the log WITH the path where the file
/// was left: it is the only thing left for the reader to find it by.
///
/// The journal sees ONE `Renamed` from `from` to `to`, which is what
/// happened: the intermediate name existed for nobody but these two calls,
/// and putting it in the journal would make undo go through it. Its undo
/// needs the same detour, and has it (`undo::rename_por_rodeo`): by
/// identity, because on a folding volume the starting name "is occupied" by
/// the file itself.
///
/// The human approved TWO names and three existed. The third falls in the
/// same directory as the other two —the policy gate was consulted about
/// that parent— and lives as long as two renames take, but it is worth
/// stating.
async fn spelling_rename(
    src: &dyn Provider,
    from: &VPath,
    to: &VPath,
    from_id: Option<norte_vfs::NodeId>,
    observer: &Arc<dyn MutationObserver>,
    ctx: &TaskCtx,
) -> Result<(), Error> {
    let step = spelling_detour(src, from, &ctx.cancel).await?;
    rename_retrying(src, from, &step, from_id, &ctx.cancel).await?;
    // From here to the end, with a CLEAN token: see "the window is not cancelable".
    let uncancelable = CancellationToken::new();
    if let Err(e) = rename_retrying(src, &step, to, from_id, &uncancelable).await {
        if let Err(rollback) = rename_retrying(src, &step, from, from_id, &uncancelable).await {
            tracing::error!(
                error = %e,
                rollback = %rollback,
                left_at = %crate::engine::span_path(&step),
                "a spelling change could neither finish nor return to its name"
            );
        }
        return Err(e);
    }
    observer
        .on_mutation(
            &Mutation::Renamed {
                from,
                to,
                batch: None,
            },
            &ctx.actor,
        )
        .await?;
    Ok(())
}

/// The FREE intermediate name for a spelling change, in `from`'s directory.
///
/// Climbs `n` until the name does not exist: a real file can be named like
/// one of our temporaries, and renaming over it would delete it. The cap is
/// generous and exhausting it is an honest error, not a loop.
async fn spelling_detour(
    src: &dyn Provider,
    from: &VPath,
    cancel: &CancellationToken,
) -> Result<VPath, Error> {
    for n in 0..1000u32 {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let mut name = crate::rename::naming::TEMP_PREFIX.to_vec();
        name.extend_from_slice(format!("case-{n}").as_bytes());
        let seg = Segment::new(name).map_err(|_| Error::InvalidPath)?;
        let cand = from.with_file_name(seg).ok_or(Error::InvalidPath)?;
        match with_retry(cancel, || src.stat(&cand).boxed()).await {
            Err(Error::NotFound) => return Ok(cand),
            Ok(_) => {}
            Err(e) => return Err(e),
        }
    }
    Err(Error::Conflict {
        conflict: ConflictKind::Exists,
    })
}

/// `rename` with retries and disambiguation (issue #17): after a transient
/// failure, a retry's `NotFound`/`Conflict` is verified by IDENTITY
/// (`from_id`, captured by the caller BEFORE the first attempt): destination
/// = original node AND source absent ⇒ the rename applied. With no identity
/// nothing is guessed: the original transient error surfaces (fail-safe; the
/// user retries against the real state).
async fn rename_retrying(
    p: &dyn Provider,
    from: &VPath,
    to: &VPath,
    from_id: Option<NodeId>,
    cancel: &CancellationToken,
) -> Result<(), Error> {
    let mut attempt = 0u32;
    let mut last_transient: Option<Error> = None;
    loop {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let err = match p.rename(from, to).await {
            Ok(()) => return Ok(()),
            Err(e) => e,
        };
        match err {
            Error::NotFound | Error::Conflict { .. } if last_transient.is_some() => {
                return match rename_applied(p, from, to, from_id, cancel).await? {
                    Some(true) => Ok(()),
                    // Verified: it did NOT apply — the error is genuine (the
                    // caller's collision policy is still working).
                    Some(false) => Err(err),
                    // Unverifiable: the original transient is the truth.
                    None => Err(last_transient.take().unwrap_or(err)),
                };
            }
            e if attempt < MAX_RETRIES && is_transient(&e) && !cancel.is_cancelled() => {
                last_transient = Some(e);
                backoff_or_cancel(cancel, attempt).await?;
                attempt += 1;
            }
            e => return Err(e),
        }
    }
}

/// Did the rename really apply? `Some(true)` = the destination IS the
/// original node and the source no longer exists. `Some(false)` = verified
/// that it did NOT (the destination is ANOTHER node with the source still
/// alive, or the "destination" is a hardlink of the source — same id but
/// source present: that is not an applied rename). `None` = unverifiable
/// (no identity).
async fn rename_applied(
    p: &dyn Provider,
    from: &VPath,
    to: &VPath,
    from_id: Option<NodeId>,
    cancel: &CancellationToken,
) -> Result<Option<bool>, Error> {
    let Some(expected) = from_id else {
        return Ok(None);
    };
    let to_id = match with_retry(cancel, || p.node_id(to, FollowLinks::No).boxed()).await {
        Ok(Some(id)) => id,
        // No identity for the destination (or destination absent): unverifiable.
        Ok(None) | Err(Error::NotFound) => return Ok(None),
        Err(e) => return Err(e),
    };
    let from_gone = match with_retry(cancel, || p.stat(from).boxed()).await {
        Err(Error::NotFound) => true,
        Ok(_) => false,
        Err(e) => return Err(e),
    };
    match (to_id == expected, from_gone) {
        (true, true) => Ok(Some(true)),
        // Source alive: there was no rename — either the destination is
        // ANOTHER node (a real conflict) or it is a HARDLINK of the source
        // (same id, but an applied rename would have made the source's
        // dirent disappear).
        (_, false) => Ok(Some(false)),
        // Source gone and destination unrelated: unrecognizable state.
        (false, true) => Ok(None),
    }
}

/// Result of placing ONE leaf (file or symlink) at the destination.
#[derive(Debug, PartialEq, Eq)]
enum Placed {
    /// Transferred (maybe under an alternative name, `RenameAuto`).
    Done,
    /// Skipped by policy: the destination was not touched; in a move, the
    /// SOURCE must be kept.
    Skipped,
}

/// Does the policy tolerate the destination dir already existing (merge)?
/// `Fail`/`Ask` keep M0's strict behavior.
fn merge_allowed(p: CollisionPolicy) -> bool {
    !matches!(p, CollisionPolicy::Fail | CollisionPolicy::Ask)
}

/// Alternative name #`n`: ` (n)` suffix before the LAST extension (split at
/// the last `.` that is not the first byte — a dotfile has no extension).
/// Byte-safe: never decodes the name.
fn rename_auto_candidate(name: &[u8], n: u32) -> Vec<u8> {
    let dot = name.iter().rposition(|&b| b == b'.').filter(|&i| i > 0);
    let (stem, ext) = match dot {
        Some(i) => (&name[..i], &name[i..]),
        None => (name, &[][..]),
    };
    let mut out = stem.to_vec();
    out.extend_from_slice(format!(" ({n})").as_bytes());
    out.extend_from_slice(ext);
    out
}

/// Do `from` and `to` point to the SAME provider node? Overwriting something
/// with itself DESTROYS it (remove + read → NotFound): it has to be rejected
/// beforehand.
///
/// With real identity ([`Provider::node_id`], issue #16) the verdict is
/// DEFINITIVE both ways: equal ids = same node (even if the FS folds
/// case/normalization wider than any heuristic); different ids = different
/// nodes (even if the names only differ in case — the case-sensitive NTFS
/// under WSL case, which the heuristic used to block wrongly). With no
/// identity (`Ok(None)`), it degrades to M1's conservative heuristic. A real
/// `node_id` error aborts (fail-safe: when in doubt, nothing destructive).
///
/// `follow_src`: under `SymlinkPolicy::Follow` what is copied is the
/// source's TARGET — copying `ln → f` with `ln` pointing to `f` is
/// overwriting `f` with itself (the Overwrite's remove would destroy it
/// before reading it through the link): the source's identity is compared
/// RESOLVED (encoding-auditor finding, M2 phase 1).
async fn same_node(
    from: &VPath,
    to: &VPath,
    dst: &dyn Provider,
    follow_src: FollowLinks,
    cancel: &CancellationToken,
) -> Result<bool, Error> {
    if from == to {
        return Ok(true);
    }
    if from.scheme() != to.scheme() || from.authority() != to.authority() {
        return Ok(false);
    }
    let from_id = match with_retry(cancel, || dst.node_id(from, follow_src).boxed()).await {
        Ok(id) => id,
        // No source (or broken link): no self-destruction possible; its
        // later stat/read will give the honest error.
        Err(Error::NotFound) => return Ok(false),
        Err(e) => return Err(e),
    };
    if let Some(a) = from_id {
        match with_retry(cancel, || dst.node_id(to, FollowLinks::No).boxed()).await {
            Ok(Some(b)) => return Ok(a == b),
            // Free destination: nothing to destroy.
            Err(Error::NotFound) => return Ok(false),
            // Half identity (mixed volume): falls to the heuristic.
            Ok(None) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(same_node_heuristic(from, to, dst).await)
}

/// The SOURCE's identity mode according to the symlink policy: under
/// `Follow` the target is copied, so the relevant identity is the resolved
/// one.
fn follow_links_for(opts: TransferOptions) -> FollowLinks {
    if opts.symlinks == SymlinkPolicy::Follow {
        FollowLinks::Yes
    } else {
        FollowLinks::No
    }
}

/// Conservative heuristic for providers with no node identity: byte-equal
/// always; and, when the DESTINATION is case-insensitive, also the variant
/// that folds to the same thing. Real identity takes priority; this is what
/// is left when the backend does not give one.
///
/// Asks about the LOCATION and not the provider (#215): `capabilities()`
/// answers for the provider's mount, so under the same `file://` an exFAT
/// thumb drive mounted at `/mnt` used to get `/home`'s answer — and what is
/// decided with it is whether a `move` is a rename onto itself, which is a
/// COPY path. Making the function `async` costs nothing: its only caller
/// already was.
///
/// And it folds with the shared key (`norte_encoding::name_key`, ADR 0051)
/// instead of with a `to_lowercase`: std's `to_lowercase` is not any
/// filesystem's fold —it is missing #129's 22 deltas— and on an ext4 `+F` it
/// is nowhere near the fold that directory does. The key already knows which
/// one applies from the capabilities.
async fn same_node_heuristic(from: &VPath, to: &VPath, dst: &dyn Provider) -> bool {
    let mode = fold_mode_at(to, dst).await;
    if mode == norte_encoding::FoldMode::None {
        return false;
    }
    let a: Vec<&[u8]> = from.segments().collect();
    let b: Vec<&[u8]> = to.segments().collect();
    a.len() == b.len()
        && a.iter().zip(&b).all(|(x, y)| {
            x == y || norte_encoding::name_key(x, mode) == norte_encoding::name_key(y, mode)
        })
}

/// Resolves ONE leaf's collision against the DESTINATION (domain gotcha:
/// always against the destination). `Ok(Some(path))` = copy there; `Ok(None)`
/// = skipped by policy.
///
/// Takes the [`Destination`] and not the bare provider (#218): the deciding
/// `stat` and the executing `remove` go through the DESCRIPTOR when there is
/// one. By path, a substituted intermediate component made it `lstat` —and
/// then delete— a file from the attacker's tree, and only afterward would
/// the confined write be refused: a file destroyed outside the root, nothing
/// written in its place, and a journal entry naming a different location.
///
/// Documented residual TOCTOU window: between this `stat` and the later
/// remove/write the destination can change. No silent loss (the provider's
/// `write()` is create-new), but the atomic replace arrives with
/// `WriteOpts` in M2 (ADR 0005).
async fn resolve_collision(
    into: &Destination<'_>,
    to: &VPath,
    src_entry: &Entry,
    policy: CollisionPolicy,
    observer: &Arc<dyn MutationObserver>,
    ctx: &TaskCtx,
) -> Result<Option<VPath>, Error> {
    let dst = into.provider();
    let at_dest = into.at(to.clone());
    let existing = match with_retry(&ctx.cancel, || at_dest.stat().boxed()).await {
        Err(Error::NotFound) => return Ok(Some(to.clone())),
        Ok(e) => e,
        Err(e) => return Err(e),
    };
    // If the provider echoes REAL keys (MemProvider), this catches any
    // folding (case AND normalization): the "collided" one is the source itself.
    if existing.path == src_entry.path {
        return Err(Error::InvalidPath);
    }
    match policy {
        // A real `Ask` arrives with the TUI's dialogs (phase 5, ADR 0005).
        CollisionPolicy::Fail | CollisionPolicy::Ask => Err(Error::Conflict {
            conflict: ConflictKind::Exists,
        }),
        CollisionPolicy::Skip => Ok(None),
        CollisionPolicy::Overwrite => {
            overwrite_existing(&at_dest, &existing, observer, ctx).await?;
            Ok(Some(to.clone()))
        }
        CollisionPolicy::Newer => match (src_entry.mtime_ms, existing.mtime_ms) {
            (Some(s), Some(d)) if s > d => {
                overwrite_existing(&at_dest, &existing, observer, ctx).await?;
                Ok(Some(to.clone()))
            }
            (Some(_), Some(_)) => Ok(None),
            // No comparable mtime: never guess (ADR 0005).
            _ => Err(Error::Conflict {
                conflict: ConflictKind::Exists,
            }),
        },
        CollisionPolicy::RenameAuto => {
            let name = to.file_name().ok_or(Error::InvalidPath)?;
            let name = name.as_bytes().to_vec();
            for n in 1..=1000u32 {
                if ctx.cancel.is_cancelled() {
                    return Err(Error::Cancelled);
                }
                let seg = Segment::new(rename_auto_candidate(&name, n))
                    .map_err(|_| Error::InvalidPath)?;
                let cand = to.with_file_name(seg).ok_or(Error::InvalidPath)?;
                match with_retry(&ctx.cancel, || dst.stat(&cand).boxed()).await {
                    Err(Error::NotFound) => return Ok(Some(cand)),
                    Ok(_) => {}
                    Err(e) => return Err(e),
                }
            }
            Err(Error::Conflict {
                conflict: ConflictKind::Exists,
            })
        }
    }
}

/// Removes the destination's existing leaf to replace it (Overwrite/Newer).
/// Never overwrites a DIR with a leaf: that is `TypeMismatch`, not policy.
///
/// The delete goes through the DESCRIPTOR if the destination has a root
/// (#218). A root that does not know how to delete confined makes the
/// policy be REJECTED: falling back to a by-path delete would reopen the
/// hole in the one place where this operation destroys, and doing it
/// silently on top.
async fn overwrite_existing(
    dest: &Dest<'_>,
    existing: &Entry,
    observer: &Arc<dyn MutationObserver>,
    ctx: &TaskCtx,
) -> Result<(), Error> {
    if existing.kind == EntryKind::Dir {
        return Err(Error::Conflict {
            conflict: ConflictKind::TypeMismatch,
        });
    }
    dest.remove(&ctx.cancel).await?;
    observer
        .on_mutation(&Mutation::Removed(dest.path()), &ctx.actor)
        .await?;
    Ok(())
}

/// Creates the destination dir, or ACCEPTS it if it already exists as a dir
/// and the policy allows merging (spec: copying dir over dir = merge, policy
/// per leaf).
///
/// #32.2 — pre-stat of the destination BEFORE the first mkdir: it is the
/// ONLY way to distinguish, after a transient failure, our phantom dir from
/// a preexisting one — with it, the ambiguous dir's `Created` reaches the
/// journal (rule 4) and undo knows about it. Honest cost: +1 stat per NEW
/// dir; for a preexisting dir under merge it is neutral or better (the stat
/// replaces the failed mkdir + the old path's stat).
async fn ensure_dir(
    dest: &Dest<'_>,
    opts: TransferOptions,
    observer: &Arc<dyn MutationObserver>,
    ctx: &TaskCtx,
) -> Result<(), Error> {
    let to = dest.path();
    // The pre-stat goes through the DESCRIPTOR when there is one (#218): by
    // path, a substituted intermediate component made it look at the
    // attacker's tree and answer "already exists as a dir", so the merge
    // went ahead over a location the root does not cover. An `EscapesRoot`
    // comes out through the error arm below and stops the operation, which
    // is the answer.
    let pre = match with_retry(&ctx.cancel, || dest.stat().boxed()).await {
        Ok(e) => Some(e),
        Err(Error::NotFound) => None,
        Err(e) => return Err(e),
    };
    if let Some(existing) = pre {
        // Preexisting: NEVER Created (it is not ours; undo does not touch it).
        return if existing.kind != EntryKind::Dir {
            Err(Error::Conflict {
                conflict: ConflictKind::TypeMismatch,
            })
        } else if merge_allowed(opts.on_collision) {
            Ok(())
        } else {
            Err(Error::Conflict {
                conflict: ConflictKind::Exists,
            })
        };
    }
    match mkdir_retrying(dest, &ctx.cancel).await {
        Ok(()) => {
            let node = dest.node_id_for(observer).await;
            observer
                .on_mutation(&Mutation::Created { path: to, node }, &ctx.actor)
                .await?;
            Ok(())
        }
        // An escape is NOT a collision to merge: the `stat` below goes by
        // path and would find the OUTSIDE directory, so the merge would
        // proceed over a location the root does not cover.
        Err(
            e @ Error::Conflict {
                conflict: ConflictKind::EscapesRoot,
            },
        ) => Err(e),
        // Conflict with NO ambiguity: a third party created the dir between
        // our pre-stat and the mkdir (an external race). Merge absorbs it
        // with NO Created (it is not ours); Fail/Ask fail safely.
        Err(Error::Conflict { .. }) if merge_allowed(opts.on_collision) => {
            // By descriptor, same reason as the pre-stat.
            let existing = with_retry(&ctx.cancel, || dest.stat().boxed()).await?;
            if existing.kind == EntryKind::Dir {
                Ok(())
            } else {
                Err(Error::Conflict {
                    conflict: ConflictKind::TypeMismatch,
                })
            }
        }
        Err(e) => Err(e),
    }
}

/// Copies a FILE leaf applying the collision policy and whole-file-level
/// retries (a transient failure restarts the file; offset resume arrives in
/// M2).
async fn copy_file_leaf(
    src: &dyn Provider,
    into: &Destination<'_>,
    entry: &Entry,
    to: &VPath,
    opts: TransferOptions,
    observer: &Arc<dyn MutationObserver>,
    ctx: &TaskCtx,
) -> Result<Placed, Error> {
    let Some(target) = resolve_collision(into, to, entry, opts.on_collision, observer, ctx).await?
    else {
        return Ok(Placed::Skipped);
    };
    // AFTER the collision: the policy may have changed its name, and the
    // relative path has to be the one it is really written under.
    let dest = into.at(target);
    copy_file_retrying(src, &dest, &entry.path, entry.size, opts, observer, ctx).await?;
    Ok(Placed::Done)
}

/// Copies a SYMLINK leaf according to policy (ADR 0005).
async fn copy_symlink_leaf(
    src: &dyn Provider,
    into: &Destination<'_>,
    entry: &Entry,
    to: &VPath,
    opts: TransferOptions,
    observer: &Arc<dyn MutationObserver>,
    ctx: &TaskCtx,
) -> Result<Placed, Error> {
    match opts.symlinks {
        SymlinkPolicy::Skip => Ok(Placed::Skipped),
        SymlinkPolicy::Preserve => {
            let target_bytes =
                with_retry(&ctx.cancel, || src.read_link(&entry.path).boxed()).await?;
            let Some(target) =
                resolve_collision(into, to, entry, opts.on_collision, observer, ctx).await?
            else {
                return Ok(Placed::Skipped);
            };
            // `Unknown` (issue #18): the kind is resolved by the DESTINATION
            // provider best-effort against its own tree; unix ignores it for free.
            let dest = into.at(target.clone());
            symlink_retrying(&dest, &target_bytes, SymlinkKind::Unknown, &ctx.cancel).await?;
            let node = dest.node_id_for(observer).await;
            observer
                .on_mutation(
                    &Mutation::Created {
                        path: &target,
                        node,
                    },
                    &ctx.actor,
                )
                .await?;
            Ok(Placed::Done)
        }
        SymlinkPolicy::Follow => {
            // Probes the target BEFORE any destructive action (Overwrite
            // deletes the destination): a dir-symlink must fail without
            // having touched anything. Dropping the stream releases the fd
            // (tested).
            match src.read(&entry.path, None).await {
                Ok(probe) => drop(probe),
                // The link points to a DIRECTORY: following it requires
                // cycle detection (a visited set) — M2 (ADR 0005).
                Err(Error::Conflict {
                    conflict: ConflictKind::TypeMismatch,
                }) => return Err(Error::Unsupported),
                Err(e) => return Err(e),
            }
            let Some(target) =
                resolve_collision(into, to, entry, opts.on_collision, observer, ctx).await?
            else {
                return Ok(Placed::Skipped);
            };
            // Unknown size (the stat describes the LINK, not the target).
            // The target may have changed after probing: it is re-mapped just the same.
            let dest = into.at(target);
            match copy_file_retrying(src, &dest, &entry.path, None, opts, observer, ctx).await {
                Ok(()) => Ok(Placed::Done),
                Err(Error::Conflict {
                    conflict: ConflictKind::TypeMismatch,
                }) => Err(Error::Unsupported),
                Err(e) => Err(e),
            }
        }
    }
}

/// Copies `from` → `to` (recursively if a dir) with `opts`'s policies.
///
/// Cancellation/failure mid-TREE: each individual file is left complete or
/// with no trace (the sink's contract), but the subtree already copied
/// STAYS at the destination — everything committed was observed as
/// `Created` (M3's journal undo will revert it; until then it is manual
/// cleanup).
#[tracing::instrument(skip_all, fields(from = %from.display_lossy(), to = %to.display_lossy()))]
// The eighth argument is the destination's anchor (#295). Grouping it with
// `opts` would cost `TransferOptions`'s `Copy`, which is copied at every
// step of a tree; grouping it with the providers would mix the WHAT with
// the WHERE.
#[expect(
    clippy::too_many_arguments,
    reason = "the options travel loose: with the providers they would mix the WHAT with the WHERE"
)]
pub(crate) async fn copy_task(
    src: Arc<dyn Provider>,
    dst: Arc<dyn Provider>,
    from: VPath,
    to: VPath,
    opts: TransferOptions,
    dest_anchor: Option<norte_proto::DirAnchor>,
    observer: Arc<dyn MutationObserver>,
    ctx: &TaskCtx,
) -> Result<(), Error> {
    if ctx.cancel.is_cancelled() {
        return Err(Error::Cancelled);
    }
    // The journal's verdict, fixed BEFORE the first effect (#205): this Task
    // stays entirely inside the journal or entirely outside it. A
    // `copy_tree` is the example the issue came from.
    let observer = crate::observer::pin_for_task(observer).await?;
    // Copying a dir INSIDE itself would produce an absurd nested copy;
    // copying something ONTO ITSELF with Overwrite would destroy it
    // (finding B1). Under Follow, "itself" is the source's resolved TARGET.
    if Arc::ptr_eq(&src, &dst)
        && (is_descendant_folded(&to, &from, &*dst).await
            || same_node(&from, &to, &*dst, follow_links_for(opts), &ctx.cancel).await?)
    {
        return Err(Error::InvalidPath);
    }
    let src_entry = with_retry(&ctx.cancel, || src.stat(&from).boxed()).await?;
    let task = ctx.progress.snapshot().task_id.get();
    match src_entry.kind {
        EntryKind::File => {
            ctx.progress.update(|p| {
                p.bytes_total = src_entry.size;
                p.entries_total = Some(1);
                p.current = Some(from.clone());
            });
            // The leaf is confined under ITS destination DIRECTORY (#219),
            // which is what the human chose and what the gate resolved. It
            // used to go unconfined, with the argument that "a lone leaf
            // does not hang from any approved tree"; the security review
            // showed it does hang from one, and that without it `fs.copy`
            // —the product's most common operation— had #164 intact.
            //
            // Opened INSIDE each leaf arm, not before the `match`: the
            // third one —a dir-symlink under `Follow`— is diverted to
            // `copy_tree`, which opens its own, and paying here for a root
            // it discards would also make it inherit its errors.
            let (dir, root) =
                open_leaf_root(&*dst, &to, dest_anchor.as_ref(), task, &ctx.cancel).await?;
            let into = leaf_destination(&*dst, dir.as_ref(), root.as_deref(), &to);
            copy_file_leaf(&*src, &into, &src_entry, &to, opts, &observer, ctx).await?;
            leaf_with_its_destination_standing(&*dst, dir.as_ref(), root.as_deref(), &ctx.cancel)
                .await?;
            ctx.progress.update(|p| p.entries_done = 1);
            Ok(())
        }
        EntryKind::Symlink => {
            // A root dir-symlink with Follow: the target's TREE is copied as
            // a real dir (issue #19), not as a leaf.
            if opts.symlinks == SymlinkPolicy::Follow
                && probe_symlink_target(&*src, &from, &ctx.cancel).await? == TargetKind::Dir
            {
                anchor_parent_or_fail(&*dst, &to, dest_anchor.as_ref(), task, &ctx.cancel).await?;
                let mut plan = walk_following(&*src, &from, true, &ctx.cancel).await?;
                hydrate_plan(&*src, &mut plan, ctx).await?;
                return copy_tree(&src, &dst, &from, &to, &plan, opts, &observer, ctx)
                    .await
                    .map(|_skipped| ());
            }
            ctx.progress.update(|p| {
                p.entries_total = Some(1);
                p.current = Some(from.clone());
            });
            let (dir, root) =
                open_leaf_root(&*dst, &to, dest_anchor.as_ref(), task, &ctx.cancel).await?;
            let into = leaf_destination(&*dst, dir.as_ref(), root.as_deref(), &to);
            copy_symlink_leaf(&*src, &into, &src_entry, &to, opts, &observer, ctx).await?;
            leaf_with_its_destination_standing(&*dst, dir.as_ref(), root.as_deref(), &ctx.cancel)
                .await?;
            ctx.progress.update(|p| p.entries_done = 1);
            Ok(())
        }
        EntryKind::Dir => {
            // A tree creates its destination, so here the anchor is checked
            // on the PARENT and by path (see `anchor_parent_or_fail`): the
            // root that confines everything else does not exist yet.
            anchor_parent_or_fail(&*dst, &to, dest_anchor.as_ref(), task, &ctx.cancel).await?;
            let mut plan = plan_for(&*src, &from, opts, &ctx.cancel).await?;
            hydrate_plan(&*src, &mut plan, ctx).await?;
            copy_tree(&src, &dst, &from, &to, &plan, opts, &observer, ctx)
                .await
                .map(|_skipped| ())
        }
        EntryKind::Other => Err(Error::Unsupported),
    }
}

/// #52: the local listing is lazy (`size`/`mtime_ms` at `None`). Progress
/// (`bytes_total`), `CollisionPolicy::Newer`, and preserving the symlink's
/// mtime need the metadata BEFORE copying: it stats ONLY the leaves missing
/// something (a `Symlink` never has `size` — it is only missing
/// `mtime_ms`, so it is not re-stated for that). A failed stat leaves
/// `None`: the bar is underestimated and, under `CollisionPolicy::Newer`,
/// the collision degrades to `Conflict{Exists}` (with no comparable mtime
/// nothing is guessed, ADR 0005) — fail-closed, never data loss.
async fn hydrate_plan(
    src: &dyn Provider,
    plan: &mut [PlanEntry],
    ctx: &TaskCtx,
) -> Result<(), Error> {
    for pe in plan.iter_mut() {
        if ctx.cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let e = &mut pe.entry;
        let needs = match e.kind {
            EntryKind::File => e.size.is_none() || e.mtime_ms.is_none(),
            EntryKind::Symlink => e.mtime_ms.is_none(),
            _ => false,
        };
        if needs {
            // Progress only when there is REALLY a stat involved: on a
            // non-lazy provider `needs` is almost always false and we do
            // not want 100k bar updates that add nothing (MINOR-5).
            ctx.progress.update(|p| p.current = Some(e.path.clone()));
            if let Ok(st) = src.stat(&e.path).await {
                e.size = e.size.or(st.size);
                e.mtime_ms = e.mtime_ms.or(st.mtime_ms);
            }
        }
    }
    Ok(())
}

/// Is the destination directory STILL the one that was opened?
///
/// A sibling of [`same_root_or_fail`] and separated from it on purpose,
/// because they answer different questions at different times:
///
/// - `same_root_or_fail` runs when OPENING the root and its failure is
///   `EscapesRoot`: the path leads elsewhere, usually via a link, and
///   writing there would mean leaving what the caller named.
/// - this one runs DURING the task and its failure is `DestinationGone`: the
///   path leads nowhere, or leads to a different directory. The folder is
///   gone.
///
/// The difference is not cosmetic. `EscapesRoot` reads like a security
/// problem and a bare `NotFound` —which is what this case used to
/// answer— reads, in the middle of a copy of thousands of files, like
/// "something from the source is missing". What really happened is that
/// someone deleted the destination folder, and that is fixed by re-creating
/// it and retrying.
///
/// The destination disappearing is NOT a far-fetched case and does not need
/// to happen from within norte: a `rm` from another terminal, another file
/// manager, or another machine over the same mount is enough.
pub(crate) async fn dest_still_there_or_fails(
    dst: &dyn Provider,
    root: &dyn norte_vfs::ConfinedRoot,
    path: &VPath,
    cancel: &CancellationToken,
) -> Result<(), Error> {
    let opened = root.root_id().await?;
    // Resolved FOLLOWING links, and that is the difference from
    // `same_root_or_fail`, which does not follow them. They are two
    // different questions:
    //
    // - that one asks whether where there was a directory there is now a
    //   planted LINK, and for that the link itself has to be looked at;
    // - this one asks whether the folder I have open is still reached
    //   through the path I was given, which is a question about the
    //   DESTINATION.
    //
    // The root is opened with `O_PATH|O_DIRECTORY` with no `O_NOFOLLOW`, so
    // `root_id` is already the node the link points to. Looking at the link,
    // a `~/backups -> /mnt/disk/backups` —or a macOS `/tmp`— would NEVER
    // match and every check would answer "the destination is gone" about a
    // destination that was perfectly fine. Following it, all four cases come
    // out right: a real directory same as before; an intact link matches; a
    // link whose target went to the trash is broken and answers `NotFound`,
    // which is the correct answer; and a link repointed elsewhere gives a
    // different node, which is also correct — the place you named is no
    // longer that one.
    let at_path = match with_retry(cancel, || dst.node_id(path, FollowLinks::Yes).boxed()).await {
        Ok(id) => id,
        // The path no longer resolves: that IS the case, and saying
        // `NotFound` would let the reader confuse it with a missing source file.
        Err(Error::NotFound) => {
            return Err(Error::Conflict {
                conflict: ConflictKind::DestinationGone,
            });
        }
        Err(e) => return Err(e),
    };
    // With no identity on either side nothing is asserted: a provider that
    // does not know how to give a `node_id` cannot deny anything, and
    // refusing here would break copies that work.
    let (Some(opened), Some(at_path)) = (opened, at_path) else {
        return Ok(());
    };
    if opened == at_path {
        return Ok(());
    }
    tracing::warn!(
        dest = %crate::engine::span_path(path),
        "the destination directory is no longer the same one: stopping instead of \
         continuing to fill one the reader no longer sees"
    );
    Err(Error::Conflict {
        conflict: ConflictKind::DestinationGone,
    })
}

/// The same thing a TREE does before saying it completed, for a lone leaf
/// (#367).
///
/// `open_leaf_root` checks the root once, when opening it — which was
/// `copy_tree`'s exact shape before ADR 0151. From there the leaf is written
/// and published through that descriptor, so deleting the destination folder
/// with the copy in flight left the file in the trash and the task saying
/// "completed".
///
/// Runs AFTER copying and not before publishing, on purpose: a tree also
/// publishes to the folder that is gone everything it copied between two
/// checks, and being stricter with a lone file than with a tree would be a
/// pointless difference someone would have to explain. What neither of the
/// two does is say it went well.
///
/// With no confined root —a backend that does not know how— there is
/// nothing to check and nothing is invented: it is the same treatment as in
/// `copy_tree`.
///
/// A destination directory that is a LINK is **not** exempt, and that was a
/// bug in the first attempt: the exception `open_leaf_root` has when opening
/// was copied over, without seeing that there it exists because that
/// question is a different one. Here the link is followed, and then an
/// intact `~/backups` matches, one whose target went to the trash answers
/// that it is not there, and one repointed says it is no longer the same
/// place. All three correct answers; exempt, the first was correct by
/// coincidence and the other two were lost.
async fn leaf_with_its_destination_standing(
    dst: &dyn Provider,
    dir: Option<&VPath>,
    root: Option<&dyn norte_vfs::ConfinedRoot>,
    cancel: &CancellationToken,
) -> Result<(), Error> {
    let (Some(dir), Some(root)) = (dir, root) else {
        return Ok(());
    };
    dest_still_there_or_fails(dst, root, dir, cancel).await
}

/// Every how many ENTRIES it re-checks that the destination is still there.
///
/// Not on every one because it is two hops to `spawn_blocking` (the
/// descriptor's `fstat` and the path's resolution) and there are trees of a
/// hundred thousand small files. Spread over 32 it is negligible time next
/// to opening, writing, and closing each file.
pub(crate) const CHECK_ROOT_EACH: usize = 32;

/// …and every how much TIME, which is the other trigger and is needed.
///
/// Counting only entries bounds the damage in files and not in bytes or wall
/// clock: a plan of ten fifty-gigabyte files is checked on the first one and
/// at the end, so it could spend five hours filling a folder that no longer
/// exists. With the time limit, between two huge files it is checked just
/// the same, and the two conditions together cover both extremes: many tiny
/// files and few huge ones.
///
/// **It does NOT bound to five seconds, and it is best not read that way.**
/// The condition is checked once per ENTRY, at the top of the loop, so what
/// it guarantees is "as soon as the current entry finishes, if five seconds
/// have already passed". A fifty-gigabyte file is copied whole before anyone
/// looks again. What the time limit really buys is that a plan of a few huge
/// files gets checked BETWEEN them instead of only at the start and the end
/// —which was the hole— and not a five-second ceiling on blind writing.
/// Putting it inside a single file's copy would be another decision, with
/// another cost, and ADR 0151 did not make it.
pub(crate) const CHECK_ROOT_EVERY_SECONDS: u64 = 5;

/// Copies the `from` → `to` tree following an ALREADY walked plan (the walk
/// is the caller's: move reuses it for the delete — issue #9). The copy
/// ignores provenance (a synthetic dir from an expanded link is created as a
/// real dir, issue #19); provenance rules in the move's DELETE. Returns the
/// SOURCE paths skipped by policy (move must not delete them).
#[expect(
    clippy::too_many_arguments,
    reason = "internal function of the module, not API"
)]
async fn copy_tree(
    src: &Arc<dyn Provider>,
    dst: &Arc<dyn Provider>,
    from: &VPath,
    to: &VPath,
    plan: &[PlanEntry],
    opts: TransferOptions,
    observer: &Arc<dyn MutationObserver>,
    ctx: &TaskCtx,
) -> Result<Vec<VPath>, Error> {
    let bytes_total: u64 = plan
        .iter()
        .filter(|pe| pe.entry.kind == EntryKind::File)
        .filter_map(|pe| pe.entry.size)
        .sum();
    let total = plan.len() as u64 + 1; // +1 for the root
    ctx.progress.update(|p| {
        p.bytes_total = Some(bytes_total);
        p.entries_total = Some(total);
    });

    // The tree's root is created by PATH: its parent is not a location this
    // destination has a root for, and creating it is exactly what gives the
    // root that confines everything else.
    ensure_dir(&Dest::plain(&**dst, to.clone()), opts, observer, ctx).await?;
    ctx.progress.update(|p| {
        p.entries_done += 1;
        p.current = Some(to.clone());
    });
    // And from here on, EVERYTHING hangs from it: it is opened once per
    // operation (#164) and every step routes its relative path. A
    // destination that does not know how to confine itself says so in the
    // log and continues by the usual path; one that said it knew and could
    // not STOPS the copy (see `open_dest_root`).
    let root = open_dest_root(&**dst, to, ctx.progress.snapshot().task_id.get()).await?;
    // And that the opened root is the one just created, not another.
    if let Some(root) = root.as_deref() {
        same_root_or_fail(&**dst, root, to, &ctx.cancel).await?;
    }
    let into = Destination::new(&**dst, root.as_deref(), to);

    let mut skipped: Vec<VPath> = Vec::new();
    let mut last_check = std::time::Instant::now();
    for (i, pe) in plan.iter().enumerate() {
        // Between one entry and the next it can be PAUSED (ADR 0147): the
        // only place where a copy with no chunks (server-to-server) stops.
        ctx.checkpoint().await?;
        // And it is checked again that the opened root STILL is the path
        // that was requested to copy to.
        //
        // Checking only when it was opened is not enough, and this is a bug
        // a reader found: they set a large folder to copy and, with the bar
        // running, deleted the destination one. norte deletes to the trash,
        // i.e. a `rename`, and a `rename` does NOT invalidate the
        // descriptor: the directory stays alive with the same inode
        // elsewhere, so the copy kept happily filling it and the task ended
        // up saying "completed". The files were in the trash. Saying "done"
        // there is worse than failing, because nobody is going to check.
        // `i > 0`: on the first lap the check above just ran, with only a
        // `Destination::new` and no I/O in between. Without this, every
        // copy —including one of a SINGLE entry— paid for the check three
        // times and was exposed once more to a false positive.
        if let Some(root) = root.as_deref()
            && i > 0
            && (i % CHECK_ROOT_EACH == 0
                || last_check.elapsed().as_secs() >= CHECK_ROOT_EVERY_SECONDS)
        {
            dest_still_there_or_fails(&**dst, root, to, &ctx.cancel).await?;
            last_check = std::time::Instant::now();
        }
        let entry = &pe.entry;
        let target = rebase(&entry.path, from, to)?;
        ctx.progress
            .update(|p| p.current = Some(entry.path.clone()));
        match entry.kind {
            EntryKind::Dir => {
                ensure_dir(&into.at(target), opts, observer, ctx).await?;
            }
            EntryKind::File => {
                if copy_file_leaf(&**src, &into, entry, &target, opts, observer, ctx).await?
                    == Placed::Skipped
                {
                    // The bar must be able to reach 100%: what was skipped does not count.
                    ctx.progress.update(|p| {
                        p.bytes_total = p
                            .bytes_total
                            .map(|t| t.saturating_sub(entry.size.unwrap_or(0)));
                    });
                    skipped.push(entry.path.clone());
                }
            }
            EntryKind::Symlink => {
                if copy_symlink_leaf(&**src, &into, entry, &target, opts, observer, ctx).await?
                    == Placed::Skipped
                {
                    skipped.push(entry.path.clone());
                }
            }
            EntryKind::Other => return Err(Error::Unsupported),
        }
        ctx.progress.update(|p| p.entries_done += 1);
    }
    // And one last time BEFORE saying it completed, whatever the stat costs:
    // the destination disappearing with the last file in flight is exactly
    // the case the periodic check skips, and it is also the only moment
    // where lying has consequences — a "completed" closes everything and
    // nobody looks again.
    if let Some(root) = root.as_deref() {
        dest_still_there_or_fails(&**dst, root, to, &ctx.cancel).await?;
    }
    Ok(skipped)
}

/// Copies ONE file with file-level retries. With `resume=Off` a transient
/// failure restarts the whole file (the sink aborted cleanly) and returns
/// progress to the starting point. With `resume=On` the partial SURVIVES
/// (`keep`) and the retry continues from where it was (`open_resumable`) —
/// the `before` that gets restored is the file's base, not zero, and
/// `copy_file` recomposes `base + already` on every attempt.
pub(crate) async fn copy_file_retrying(
    src: &dyn Provider,
    dest: &Dest<'_>,
    from: &VPath,
    known_size: Option<u64>,
    opts: TransferOptions,
    observer: &Arc<dyn MutationObserver>,
    ctx: &TaskCtx,
) -> Result<(), Error> {
    let base = ctx.progress.snapshot().bytes_done;
    let mut attempt = 0u32;
    loop {
        match copy_file(src, dest, from, known_size, base, opts, observer, ctx).await {
            Err(e) if attempt < MAX_RETRIES && is_transient(&e) && !ctx.cancel.is_cancelled() => {
                // The file's base: `copy_file` recomposes `base + already`
                // (with resume, `already` grows; without resume, it goes back to 0).
                ctx.progress.update(|p| p.bytes_done = base);
                let delay = std::time::Duration::from_millis(BACKOFF_BASE_MS << attempt);
                attempt += 1;
                tokio::select! {
                    () = ctx.cancel.cancelled() => return Err(Error::Cancelled),
                    () = tokio::time::sleep(delay) => {}
                }
            }
            other => return other,
        }
    }
}

/// SHA-256 of the SOURCE's first `len` bytes (#35, `VerifyPolicy::Hash`):
/// compared against the destination's staging digest to decide whether the
/// partial is still valid. Reads `source[0..len]` by stream (the same cost
/// Length saves: Hash re-READS the source's prefix, but does not
/// re-WRITE it).
///
/// `Ok(Some(d))` = the prefix's digest; `Ok(None)` = the source is SHORTER
/// than `len` (no prefix to match → the caller discards). A REAL read error
/// (transient, permissions) PROPAGATES with `Err` — never confused with
/// "short source", which would destroy the partial (reviewer's M1). Checks
/// cancellation per chunk (rule 3, reviewer's M2): a GiB partial does not
/// block the Task.
async fn hash_source_prefix(
    src: &dyn Provider,
    from: &VPath,
    len: u64,
    ctx: &TaskCtx,
) -> Result<Option<[u8; 32]>, Error> {
    use sha2::{Digest, Sha256};
    let range = norte_proto::ByteRange {
        offset: 0,
        len: Some(len),
    };
    let mut stream = src.read(from, Some(range)).await?;
    let mut hasher = Sha256::new();
    let mut seen: u64 = 0;
    while let Some(item) = stream.next().await {
        if ctx.cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let chunk = item?;
        // The source could deliver more if it ignores `len`: trims to the
        // exact prefix so the digest covers ONLY `source[..len]`.
        let take = usize::try_from(len - seen)
            .unwrap_or(chunk.len())
            .min(chunk.len());
        hasher.update(&chunk[..take]);
        seen += take as u64;
        if seen >= len {
            break;
        }
    }
    if seen < len {
        return Ok(None); // source shorter than the partial
    }
    Ok(Some(hasher.finalize().into()))
}

/// Discard the resumable partial and start from scratch? Decided according
/// to `VerifyPolicy` (#35): Length compares sizes; Hash compares the
/// source's prefix digest with the staging's (if the provider exposes it, if
/// not it degrades to Length).
async fn should_discard_partial(
    src: &dyn Provider,
    dest: &Dest<'_>,
    from: &VPath,
    already: u64,
    known_size: Option<u64>,
    verify: VerifyPolicy,
    ctx: &TaskCtx,
) -> Result<bool, Error> {
    // A partial longer than the source never adds up (both policies), and it
    // saves hashing: the source changed/shrank.
    if known_size.is_some_and(|size| already > size) {
        return Ok(true);
    }
    if already == 0 || verify == VerifyPolicy::Length {
        return Ok(false);
    }
    // Hash: with no staging digest the provider does not allow verifying →
    // degrades to Length (the size check above already applied).
    // By DESCRIPTOR when there is a root: by path, this resolves the
    // staging's name by following each component, so the ONLY verification
    // there is over the bytes being resumed used to go through the door the
    // confinement closed for `stat` and `remove` — and a substituted
    // intermediate component gives it another file's digest.
    let Some(partial_dig) = dest.partial_digest(already).await? else {
        return Ok(false);
    };
    // `None` = source shorter than the partial → discard. A REAL error
    // propagates (`?`): never swallowed as "discard" (reviewer's M1).
    match hash_source_prefix(src, from, already, ctx).await? {
        Some(src_dig) => Ok(src_dig != partial_dig),
        None => Ok(true),
    }
}

/// Copies ONE file: `copy_native` if the provider (the same on both sides)
/// declares `SERVER_COPY`; if not, streaming with per-chunk cancellation.
///
/// `base` = `bytes_done` BEFORE this file (to recompose progress on resume).
/// With resume: opens `open_resumable`, discards the partial if it does not
/// match the source (`verify`, #35), reads the source from `already`, and
/// on cancellation/failure KEEPS the partial (`keep`) instead of aborting.
#[expect(
    clippy::too_many_arguments,
    reason = "internal function of the module, not API"
)]
async fn copy_file(
    src: &dyn Provider,
    dest: &Dest<'_>,
    from: &VPath,
    known_size: Option<u64>,
    base: u64,
    opts: TransferOptions,
    observer: &Arc<dyn MutationObserver>,
    ctx: &TaskCtx,
) -> Result<(), Error> {
    let (backend, to) = (dest.provider(), dest.path());
    if std::ptr::eq(
        std::ptr::from_ref(src).cast::<()>(),
        std::ptr::from_ref(backend).cast::<()>(),
    ) && src
        .capabilities()
        .flags
        .contains(norte_proto::CapabilityFlags::SERVER_COPY)
    {
        // Rule 3 (#51): copy_native is ONE potentially minutes-long await
        // (S3 multipart copy, opendal chunks it on its own) — it is raced
        // against cancellation. `biased` with the copy FIRST: a completion
        // already observed is ALWAYS journaled even if the token is also
        // cancelled (rule 4); cancellation loses no latency (the select
        // polls both branches on every wakeup). Dropping the future halfway
        // never publishes a HALF object (CopyObject is atomic; an
        // incomplete multipart publishes nothing), but two documented
        // ambiguities remain (contract in `Provider::copy_native`'s
        // rustdoc):
        // - orphaned billable parts on S3: opendal only aborts the
        //   multipart on its error path, not on drop — the same case as the
        //   write sink's Drop (ADR 0016 E: the bucket's lifecycle rule for
        //   AbortIncompleteMultipartUpload);
        // - if the server completes the copy AFTER the drop, the
        //   destination is left with the WHOLE object with no journal entry
        //   (post-effect ambiguity, same family as #32) — never an
        //   unmarked partial.
        let native = tokio::select! {
            biased;
            res = src.copy_native(from, to) => res,
            () = ctx.cancel.cancelled() => return Err(Error::Cancelled),
        };
        if let Some(res) = native {
            res?;
            // The size was already given by the source's stat: zero extra round trips.
            let size = known_size.unwrap_or(0);
            ctx.progress.update(|p| p.bytes_done = base + size);
            let node = dest.node_id_for(observer).await;
            observer
                .on_mutation(&Mutation::Created { path: to, node }, &ctx.actor)
                .await?;
            return Ok(());
        }
        // `None`: the provider declined despite the flag — falls to streaming.
    }

    // Resume AGNOSTIC of the provider (ADR 0012 A2): `open_resumable` with
    // its safe default `(write, 0)` degrades cleanly on a provider with no
    // real resumption; it is not gated by capability (S3 resumes via
    // multipart, not via APPEND — rust-reviewer's M1).
    // A confined destination does NOT resume (`Dest::resumes`): keeping its
    // ephemeral staging would leave a `.norte-partial` per attempt that
    // nobody continues.
    let resume = opts.resume == norte_proto::ResumePolicy::On && dest.resumes();
    // Opens the sink: resumable (with an already durable offset) or fresh.
    let (mut sink, already) = if resume {
        let (sink, already) = dest.open_resumable().await?;
        // Discard the partial and start from scratch? The source may have
        // changed under our feet between invocations (ADR 0012, #35):
        //   - Length: a partial longer than the source does not add up.
        //   - Hash: the `source[..already]` prefix does not match the
        //     partial's byte-for-byte. If the provider does not expose a
        //     staging digest, it DEGRADES to Length (documented in the
        //     trait).
        let discard =
            should_discard_partial(src, dest, from, already, known_size, opts.verify, ctx).await?;
        if discard {
            // Propagate the abort failure (rust-reviewer's M2): swallowing
            // it and continuing would leave stale bytes and the destination
            // would come out corrupt.
            sink.abort().await?;
            let (fresh, fresh_already) = dest.open_resumable().await?;
            if fresh_already != 0 {
                // The staging is still there after the abort: cannot resume
                // cleanly — fail instead of publishing something dubious.
                return Err(Error::Io { retryable: false });
            }
            (fresh, 0)
        } else {
            (sink, already)
        }
    } else {
        (dest.write().await?, 0)
    };

    // The stretch already present counts as done immediately (the bar does
    // not go backward on resume).
    ctx.progress.update(|p| p.bytes_done = base + already);

    let range = (already > 0).then_some(norte_proto::ByteRange {
        offset: already,
        len: None,
    });
    let mut stream = match src.read(from, range).await {
        Ok(s) => s,
        Err(e) => {
            release(sink, to, resume).await;
            return Err(e);
        }
    };
    let mut written = base + already;
    while let Some(item) = stream.next().await {
        // Per-chunk cancellation: clean destination, or a resumable
        // `.norte-partial` (resume), never an unmarked half-done file. And
        // the PAUSE (ADR 0147), in the same place: it waits with the file
        // open, and cancelling during the wait cleans up the same way.
        if let Err(e) = ctx.checkpoint().await {
            release(sink, to, resume).await;
            return Err(e);
        }
        let chunk = match item {
            Ok(c) => c,
            Err(e) => {
                release(sink, to, resume).await;
                return Err(e);
            }
        };
        let n = chunk.len() as u64;
        if let Err(e) = sink.write(chunk).await {
            release(sink, to, resume).await;
            return Err(e);
        }
        written += n;
        ctx.progress.update(|p| p.bytes_done = written);
    }
    if ctx.cancel.is_cancelled() {
        release(sink, to, resume).await;
        return Err(Error::Cancelled);
    }
    // Committed file size = what was written in THIS copy (`written`) minus
    // the progress `base` prior to this file. It is the witness used to
    // disambiguate a commit that applied but returned transient (#32.1).
    let final_size = written - base;
    match sink.commit().await {
        Ok(()) => {}
        // The commit (staging→final rename) may have APPLIED before
        // returning a transient error (#32.1): without this, the retry
        // re-copies and its non-replace commit gives `Conflict` → a FAILED
        // task with the file correctly copied and NO `Created` (rule 4). It
        // is disambiguated by presence + size: if `to` exists with the
        // expected size, it was our write → counts as success. If it does
        // not appear, the commit did not apply and the transient
        // propagates (the retry re-copies cleanly).
        //
        // ASSUMPTIONS (reviewer): `to` is guaranteed FREE at the start
        // (`resolve_collision` ensures this under every policy) and there is
        // a single writer per task — so a `to` of the expected size can ONLY
        // be our write (it never claims someone else's file). Residual
        // limitations, fail-safe (they propagate the transient → as before
        // the fix): a provider whose `stat` does not report `size` (`None`),
        // or a transient episode that also exhausts the `stat`, do not
        // confirm and fall back to the failure path.
        Err(e) if is_transient(&e) => {
            // And the post-transient disambiguation too (#218): claiming as
            // ours a write that is actually outside the root would put a
            // `Created` in the journal for someone else's file, and undo
            // would send it to the trash.
            match with_retry(&ctx.cancel, || dest.stat().boxed()).await {
                // Applied: `to` exists with the expected size → falls through to Created.
                Ok(entry) if entry.size == Some(final_size) => {}
                // Cancelled during the check: propagates cancellation.
                Err(Error::Cancelled) => return Err(Error::Cancelled),
                // Did not apply (NotFound), exists but does not match, or
                // the stat failed: we do not claim it — propagates the
                // transient (the retry re-copies cleanly, or the user
                // retries against the real state).
                _ => return Err(e),
            }
        }
        Err(e) => return Err(e),
    }
    // The identity of what was just published, so undo can check it is
    // deleting its own (#369, ADR 0152). Goes through the root's descriptor
    // when there is one: this is THE place where asking by path would fail
    // exactly in the interesting case.
    let node = dest.node_id_for(observer).await;
    observer
        .on_mutation(&Mutation::Created { path: to, node }, &ctx.actor)
        .await?;
    Ok(())
}

/// Drops the sink on interruption: `keep` (keeps the resumable
/// `.norte-partial`) if there is resume, `abort` (clean destination) if not.
async fn release(sink: Box<dyn norte_vfs::ByteSink>, to: &VPath, resume: bool) {
    let res = if resume {
        sink.keep().await
    } else {
        sink.abort().await
    };
    if let Err(e) = res {
        tracing::warn!(
            path = %to.display_lossy(),
            error = %e,
            "dropping the sink failed; possible orphaned staging"
        );
    }
}

/// Move: rename if source and destination live on the SAME provider (0
/// bytes); if the provider cannot (`Unsupported`: EXDEV across mounts, a
/// remote with no rename) or it is cross-provider, copy + delete the source
/// (spec §5).
///
/// The rename is TRIED first (the provider's atomic non-replace) and the
/// collision policy is applied on its `Conflict` — that way a case-rename on
/// an insensitive FS is never confused with a real collision (the provider
/// resolves it by identity) and `Overwrite` never deletes the source itself.
///
/// The copy and the delete are driven from ONE plan (a single walk, issue
/// #9): the delete removes EXACTLY what was copied, in post-order. What was
/// skipped by policy AND what appeared after the walk survive at the
/// source — never a silent loss.
#[tracing::instrument(skip_all, fields(from = %from.display_lossy(), to = %to.display_lossy()))]
// Eighth argument: the destination's anchor (#295), see `copy_task`.
#[expect(
    clippy::too_many_arguments,
    reason = "Eighth argument: the destination's anchor (#295), see `copy_task`"
)]
pub(crate) async fn move_task(
    src: Arc<dyn Provider>,
    dst: Arc<dyn Provider>,
    from: VPath,
    to: VPath,
    opts: TransferOptions,
    dest_anchor: Option<norte_proto::DirAnchor>,
    observer: Arc<dyn MutationObserver>,
    ctx: &TaskCtx,
) -> Result<(), Error> {
    if ctx.cancel.is_cancelled() {
        return Err(Error::Cancelled);
    }
    // #205, and here it matters doubly: a move-by-copy emits `created` for
    // every entry and `removed` for every one, so a half-registered Task
    // leaves an undo that restores half the source over half the destination.
    let observer = crate::observer::pin_for_task(observer).await?;
    // The anchor, BEFORE deciding which path the move takes (#295).
    //
    // The rename does not compose new paths under the destination and so
    // does not need confinement, but it DOES resolve `to` by path once: with
    // `d/sub` turned into a link, `rename` leaves the file on the other side
    // just like a copy would. Checking here covers both paths; the copy one
    // checks again against the descriptor, which is exact.
    anchor_parent_or_fail(
        &*dst,
        &to,
        dest_anchor.as_ref(),
        ctx.progress.snapshot().task_id.get(),
        &ctx.cancel,
    )
    .await?;
    if Arc::ptr_eq(&src, &dst) {
        if is_descendant_folded(&to, &from, &*dst).await {
            return Err(Error::InvalidPath);
        }
        ctx.progress.update(|p| {
            p.entries_total = Some(1);
            p.current = Some(from.clone());
        });
        match rename_with_policy(&*src, &from, &to, opts, &observer, ctx).await {
            Ok(RenameOutcome::Renamed | RenameOutcome::SkippedByPolicy) => {
                ctx.progress.update(|p| p.entries_done = 1);
                return Ok(());
            }
            // The provider does not know how to rename THIS (EXDEV across
            // mounts is the typical case): degrade to copy+delete, like cross-provider.
            Err(Error::Unsupported) => {}
            Err(e) => return Err(e),
        }
    }
    move_by_copy(src, dst, from, to, opts, dest_anchor, observer, ctx).await
}

/// Overwrite/Newer never cross types (ADR 0005): dir over leaf or vice versa
/// is `TypeMismatch`; dir over dir degrades to copy+delete (merge) by
/// returning `Unsupported` to the rename's caller.
fn check_overwrite_kinds(src_e: &Entry, existing: &Entry) -> Result<(), Error> {
    let src_dir = src_e.kind == EntryKind::Dir;
    let dst_dir = existing.kind == EntryKind::Dir;
    if src_dir && dst_dir {
        // Dir merge: let the copy+delete path do it.
        return Err(Error::Unsupported);
    }
    if src_dir != dst_dir {
        return Err(Error::Conflict {
            conflict: ConflictKind::TypeMismatch,
        });
    }
    Ok(())
}

enum RenameOutcome {
    Renamed,
    SkippedByPolicy,
}

/// Same-provider rename applying the collision policy on the provider's
/// non-replace rename's `Conflict`.
// One arm per collision POLICY, and each with its full sequence — source
// stat, destination stat, type check, delete, rename, journal—. Splitting it
// would hide which of the five does what.
#[expect(
    clippy::too_many_lines,
    reason = "five phases in order; splitting it would hide which does what"
)]
async fn rename_with_policy(
    src: &dyn Provider,
    from: &VPath,
    to: &VPath,
    opts: TransferOptions,
    observer: &Arc<dyn MutationObserver>,
    ctx: &TaskCtx,
) -> Result<RenameOutcome, Error> {
    // The source's identity BEFORE the first attempt: it is the only thing
    // that can disambiguate a rename whose effect applied after a timeout
    // (#17). Pure best-effort — identity only VERIFIES: any error here
    // degrades to None (the rename still works as in M1 and will give its
    // own error if the problem is real).
    let from_id = with_retry(&ctx.cancel, || src.node_id(from, FollowLinks::No).boxed())
        .await
        .ok()
        .flatten();
    let first = rename_retrying(src, from, to, from_id, &ctx.cancel).await;
    let conflict = match first {
        Ok(()) => {
            observer
                .on_mutation(
                    &Mutation::Renamed {
                        from,
                        to,
                        batch: None,
                    },
                    &ctx.actor,
                )
                .await?;
            return Ok(RenameOutcome::Renamed);
        }
        Err(e @ Error::Conflict { .. }) => e,
        Err(e) => return Err(e),
    };
    // Is the "collision" the file ITSELF seen under a different spelling? (#274)
    //
    // On a folding volume —APFS, NTFS, exFAT, an ext4 `+F`— `Foo.txt` and
    // `foo.txt` are the same node, and renaming WITHOUT OVERWRITING (which is
    // how norte renames: `renameat2(RENAME_NOREPLACE)`,
    // `renamex_np(RENAME_EXCL)`, `MoveFileExW` with no replace) answers that
    // the destination already exists. But changing the spelling is REAL
    // work: the name's bytes change, the batch planner already treats it
    // that way, and the reader has no other way to do it.
    //
    // And it was not just that it got refused. With `Overwrite`, the arm
    // below deleted the destination before renaming — i.e. the file itself
    // — and then renamed something that was no longer there: a case change
    // that took the file down with it.
    //
    // Decided by IDENTITY and never by the name heuristic: what comes next
    // moves one file over another, and doing it on an assumption is exactly
    // how the wrong one gets lost. With no identity (a provider that does
    // not give one) it falls back to the usual behavior.
    if is_the_same_leaf_with_a_different_spelling(src, from, to, from_id, &ctx.cancel).await {
        spelling_rename(src, from, to, from_id, observer, ctx).await?;
        return Ok(RenameOutcome::Renamed);
    }
    match opts.on_collision {
        CollisionPolicy::Fail | CollisionPolicy::Ask => Err(conflict),
        CollisionPolicy::Skip => Ok(RenameOutcome::SkippedByPolicy),
        CollisionPolicy::Overwrite => {
            let src_e = with_retry(&ctx.cancel, || src.stat(from).boxed()).await?;
            let existing = with_retry(&ctx.cancel, || src.stat(to).boxed()).await?;
            check_overwrite_kinds(&src_e, &existing)?;
            // UNCONFINED, on purpose: an in-place rename opens no root
            // —the `renameat` that comes next could not go by descriptor
            // without one either— so there is no handle to use here and
            // faking one would be worse. What this path DOES have is
            // `from_id`, which `rename_retrying` checks.
            overwrite_existing(&Dest::plain(src, to.clone()), &existing, observer, ctx).await?;
            rename_retrying(src, from, to, from_id, &ctx.cancel).await?;
            observer
                .on_mutation(
                    &Mutation::Renamed {
                        from,
                        to,
                        batch: None,
                    },
                    &ctx.actor,
                )
                .await?;
            Ok(RenameOutcome::Renamed)
        }
        CollisionPolicy::Newer => {
            let src_e = with_retry(&ctx.cancel, || src.stat(from).boxed()).await?;
            let existing = with_retry(&ctx.cancel, || src.stat(to).boxed()).await?;
            match (src_e.mtime_ms, existing.mtime_ms) {
                (Some(s), Some(d)) if s > d => {
                    check_overwrite_kinds(&src_e, &existing)?;
                    overwrite_existing(&Dest::plain(src, to.clone()), &existing, observer, ctx)
                        .await?;
                    rename_retrying(src, from, to, from_id, &ctx.cancel).await?;
                    observer
                        .on_mutation(
                            &Mutation::Renamed {
                                from,
                                to,
                                batch: None,
                            },
                            &ctx.actor,
                        )
                        .await?;
                    Ok(RenameOutcome::Renamed)
                }
                (Some(_), Some(_)) => Ok(RenameOutcome::SkippedByPolicy),
                _ => Err(conflict),
            }
        }
        CollisionPolicy::RenameAuto => {
            let name = to.file_name().ok_or(Error::InvalidPath)?;
            let name = name.as_bytes().to_vec();
            for n in 1..=1000u32 {
                if ctx.cancel.is_cancelled() {
                    return Err(Error::Cancelled);
                }
                let seg = Segment::new(rename_auto_candidate(&name, n))
                    .map_err(|_| Error::InvalidPath)?;
                let cand = to.with_file_name(seg).ok_or(Error::InvalidPath)?;
                match rename_retrying(src, from, &cand, from_id, &ctx.cancel).await {
                    Ok(()) => {
                        observer
                            .on_mutation(
                                &Mutation::Renamed {
                                    from,
                                    to: &cand,
                                    batch: None,
                                },
                                &ctx.actor,
                            )
                            .await?;
                        return Ok(RenameOutcome::Renamed);
                    }
                    Err(Error::Conflict { .. }) => {}
                    Err(e) => return Err(e),
                }
            }
            Err(conflict)
        }
    }
}

/// Move by copy + delete with a single plan: the copy's walk IS the delete's
/// list. What was skipped by policy stays at the source (along with its
/// ancestor dirs).
// Two forms of the same verb —a leaf and a tree— each with its copy phase
// and its delete phase. Splitting them would duplicate the "inside itself"
// guard and the plan, which is where the bug would be if they were split.
#[expect(
    clippy::too_many_lines,
    reason = "copy and delete share the \"inside itself\" guard and the plan"
)]
// Eighth argument: the destination's anchor (#295), see `copy_task`.
#[expect(
    clippy::too_many_arguments,
    reason = "Eighth argument: the destination's anchor (#295), see `copy_task`"
)]
async fn move_by_copy(
    src: Arc<dyn Provider>,
    dst: Arc<dyn Provider>,
    from: VPath,
    to: VPath,
    opts: TransferOptions,
    dest_anchor: Option<norte_proto::DirAnchor>,
    observer: Arc<dyn MutationObserver>,
    ctx: &TaskCtx,
) -> Result<(), Error> {
    if Arc::ptr_eq(&src, &dst)
        && (is_descendant_folded(&to, &from, &*dst).await
            || same_node(&from, &to, &*dst, follow_links_for(opts), &ctx.cancel).await?)
    {
        return Err(Error::InvalidPath);
    }
    let src_entry = with_retry(&ctx.cancel, || src.stat(&from).boxed()).await?;
    // Does it move as a TREE? A dir always; a root dir-symlink only under
    // Follow (issue #19): its content expands at the destination and at the
    // source THE LINK is deleted.
    let tree_plan = match src_entry.kind {
        EntryKind::Dir => Some(plan_for(&*src, &from, opts, &ctx.cancel).await?),
        EntryKind::Symlink
            if opts.symlinks == SymlinkPolicy::Follow
                && probe_symlink_target(&*src, &from, &ctx.cancel).await? == TargetKind::Dir =>
        {
            Some(walk_following(&*src, &from, true, &ctx.cancel).await?)
        }
        EntryKind::File | EntryKind::Symlink => None,
        EntryKind::Other => return Err(Error::Unsupported),
    };
    match tree_plan {
        None => {
            ctx.progress.update(|p| {
                p.bytes_total = src_entry.size;
                // 2 steps: copy + delete the source.
                p.entries_total = Some(2);
                p.current = Some(from.clone());
            });
            // The same root as in `copy_task` (#219): the leaf's destination
            // directory. A move by copy WRITES just like a copy, and also
            // deletes the source afterward — leaving it unconfined was half
            // the hole with the other half right next to it.
            let (dest_dir, root) = open_leaf_root(
                &*dst,
                &to,
                dest_anchor.as_ref(),
                ctx.progress.snapshot().task_id.get(),
                &ctx.cancel,
            )
            .await?;
            let into = leaf_destination(&*dst, dest_dir.as_ref(), root.as_deref(), &to);
            let placed = if src_entry.kind == EntryKind::File {
                copy_file_leaf(&*src, &into, &src_entry, &to, opts, &observer, ctx).await?
            } else {
                copy_symlink_leaf(&*src, &into, &src_entry, &to, opts, &observer, ctx).await?
            };
            ctx.progress.update(|p| p.entries_done = 1);
            if placed == Placed::Skipped {
                // Not copied ⇒ not deleted: the source is kept.
                ctx.progress.update(|p| p.entries_done = 2);
                return Ok(());
            }
            // The destination still stands where requested, BEFORE deleting
            // the source (#367). Here `leaf_with_its_destination_standing`'s
            // reasoning is inverted: in a copy, checking after publishing
            // only changes what is said at the end; here what follows is an
            // IRREVERSIBLE delete of the source. Without this, deleting the
            // destination folder mid-copy left the bytes in the trash, the
            // source destroyed, and the task saying "completed" — worse than
            // the case that opened #367, and through the same mechanism.
            leaf_with_its_destination_standing(
                &*dst,
                dest_dir.as_ref(),
                root.as_deref(),
                &ctx.cancel,
            )
            .await?;
            if ctx.cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            remove_retrying(&*src, &from, &ctx.cancel).await?;
            observer
                .on_mutation(&Mutation::Removed(&from), &ctx.actor)
                .await?;
            ctx.progress.update(|p| p.entries_done = 2);
            Ok(())
        }
        Some(mut plan) => {
            anchor_parent_or_fail(
                &*dst,
                &to,
                dest_anchor.as_ref(),
                ctx.progress.snapshot().task_id.get(),
                &ctx.cancel,
            )
            .await?;
            hydrate_plan(&*src, &mut plan, ctx).await?;
            let skipped = copy_tree(&src, &dst, &from, &to, &plan, opts, &observer, ctx).await?;
            // Delete phase: the total grows with the delete steps (the bar
            // stays monotonic; copy_tree already counted its own).
            ctx.progress.update(|p| {
                p.entries_total = p.entries_total.map(|t| t + plan.len() as u64 + 1);
            });
            // Deletes EXACTLY what was copied, in post-order. What was
            // skipped (and its ancestors) and what appeared after the walk
            // survive: that remove is either not even attempted (skip) or
            // fails with Conflict (appeared). Provenance rules (issue #19):
            // what was seen THROUGH a link belongs to the TARGET and is
            // never deleted; from the expanded link THE LINK is deleted.
            // Documented DAG corner: a leaf reachable by two paths, skipped
            // on one and moved through the other, ends up only at the
            // destination (no loss: the content lives there).
            for pe in plan.iter().rev() {
                ctx.checkpoint().await?;
                let keep = match pe.provenance {
                    Provenance::ViaLink => true,
                    Provenance::LinkRoot => {
                        skipped.iter().any(|s| is_descendant(s, &pe.entry.path))
                    }
                    Provenance::Real => {
                        skipped.contains(&pe.entry.path)
                            || (pe.entry.kind == EntryKind::Dir
                                && skipped.iter().any(|s| is_descendant(s, &pe.entry.path)))
                    }
                };
                if keep {
                    ctx.progress.update(|p| p.entries_done += 1);
                    continue;
                }
                ctx.progress
                    .update(|p| p.current = Some(pe.entry.path.clone()));
                remove_retrying(&*src, &pe.entry.path, &ctx.cancel).await?;
                observer
                    .on_mutation(&Mutation::Removed(&pe.entry.path), &ctx.actor)
                    .await?;
                ctx.progress.update(|p| p.entries_done += 1);
            }
            if ctx.cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            if skipped.is_empty() {
                // Root: for a dir, the already empty dir; for a root
                // dir-symlink under Follow, THE LINK (remove never follows links).
                remove_retrying(&*src, &from, &ctx.cancel).await?;
                observer
                    .on_mutation(&Mutation::Removed(&from), &ctx.actor)
                    .await?;
            }
            ctx.progress.update(|p| p.entries_done += 1);
            Ok(())
        }
    }
}

/// Delete: `Trash` = ONE provider operation on the root (the OS takes away
/// the whole tree — cancelable BEFORE firing, not halfway);
/// `Permanent` = recursive post-order (children fall before their parent;
/// cancelling halfway leaves the rest of the tree intact, the root falls
/// last). ADR 0009.
///
/// # The row that does not arrive
/// An observer that fails AFTER a successful burial leaves the file moved
/// and unrecorded (#160). It is compensated with `restore_from` when the
/// trash names what it took, and it is stated in the log whether it arrives
/// or not. Same criterion, same limits, and the same TOCTOU as
/// [`crate::sync::exec::bury`]'s `restore_from`: the effect is not left
/// orphaned of its record.
#[tracing::instrument(skip_all, fields(path = %path.display_lossy(), ?mode))]
pub(crate) async fn delete_task(
    provider: Arc<dyn Provider>,
    path: VPath,
    mode: DeleteMode,
    observer: Arc<dyn MutationObserver>,
    ctx: &TaskCtx,
) -> Result<(), Error> {
    if ctx.cancel.is_cancelled() {
        return Err(Error::Cancelled);
    }
    // #205: a permanent delete walks the tree emitting one mutation per
    // entry, so it is the easiest case to split in half — and the most
    // expensive, because what carries no row cannot even be named afterward.
    let observer = crate::observer::pin_for_task(observer).await?;
    if mode == DeleteMode::Trash {
        ctx.progress.update(|p| {
            p.entries_total = Some(1);
            p.current = Some(path.clone());
        });
        // The operation's deterministic id (#99): wall clock ONCE + the
        // task's numeric id as a counter — stable across every retry.
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
        let trash_id =
            norte_vfs::trash::TrashId::new(now_ms, ctx.progress.snapshot().task_id.get());
        let dest = trash_retrying(&*provider, &path, &trash_id, &ctx.cancel).await?;
        if let Err(e) = observer
            .on_mutation(
                &Mutation::Trashed {
                    path: &path,
                    dest: dest.as_ref(),
                },
                &ctx.actor,
            )
            .await
        {
            // #160: the file is already buried and its record did not
            // arrive — hard rule 4 broken through an ordinary F8's path. It
            // is returned to its path and the delete fails with the tree as
            // it was. Same as in `sync::exec::bury`, it can only be done
            // when the trash NAMES what it takes: with `DestTrash::Opaque`
            // (macOS, Windows) the log line is what is left.
            let returned = match dest.as_ref() {
                Some(en) => provider.restore_from(en, &path).await,
                None => Err(Error::Unsupported),
            };
            tracing::error!(
                error = %e,
                buried = %crate::engine::span_path(&path),
                at = dest.as_ref().map(crate::engine::span_path),
                returned = returned.is_ok(),
                "fs.delete: the file was buried and its journal entry did NOT arrive",
            );
            return Err(e);
        }
        ctx.progress.update(|p| p.entries_done = 1);
        return Ok(());
    }
    let entry = with_retry(&ctx.cancel, || provider.stat(&path).boxed()).await?;
    if entry.kind == EntryKind::Dir {
        let entries = walk(&*provider, &path, &ctx.cancel).await?;
        ctx.progress
            .update(|p| p.entries_total = Some(entries.len() as u64 + 1));
        // The walk emits every parent before its children: walking it
        // backward IS the post-order (every dir reaches its remove empty).
        for e in entries.iter().rev() {
            ctx.checkpoint().await?;
            ctx.progress.update(|p| p.current = Some(e.path.clone()));
            remove_retrying(&*provider, &e.path, &ctx.cancel).await?;
            observer
                .on_mutation(&Mutation::Removed(&e.path), &ctx.actor)
                .await?;
            ctx.progress.update(|p| p.entries_done += 1);
        }
    } else {
        ctx.progress.update(|p| p.entries_total = Some(1));
    }
    if ctx.cancel.is_cancelled() {
        return Err(Error::Cancelled);
    }
    remove_retrying(&*provider, &path, &ctx.cancel).await?;
    observer
        .on_mutation(&Mutation::Removed(&path), &ctx.actor)
        .await?;
    ctx.progress.update(|p| p.entries_done += 1);
    Ok(())
}

/// `fs.mkdir` task (#104, F7): ONE directory, no `-p`. Pre-stat (#32) so as
/// never to claim a preexisting node as ours: ANY prior node — dir included
/// — is `Conflict{Exists}` (creating asserts a FREE name; `ensure_dir`'s
/// silent idempotence belongs to copy merges, not an F7). With the pre-stat
/// at `NotFound`, `mkdir_retrying` disambiguates transients (verifying the
/// ambiguous dir EMPTY — see its rustdoc and the residual window it
/// documents) and the `Created` reaches the journal (rule 4) when the dir is
/// ours.
pub(crate) async fn mkdir_task(
    provider: Arc<dyn Provider>,
    path: VPath,
    observer: Arc<dyn MutationObserver>,
    ctx: &TaskCtx,
) -> Result<(), Error> {
    if ctx.cancel.is_cancelled() {
        return Err(Error::Cancelled);
    }
    // A single mutation, so here there is no half to split. It is fixed just
    // the same: the rule is "every mutating Task fixes its verdict", and an
    // exception for being short is what someone extends without remembering
    // (#205).
    let observer = crate::observer::pin_for_task(observer).await?;
    ctx.progress.update(|p| {
        p.entries_total = Some(1);
        p.current = Some(path.clone());
    });
    match with_retry(&ctx.cancel, || provider.stat(&path).boxed()).await {
        Ok(_) => {
            return Err(Error::Conflict {
                conflict: ConflictKind::Exists,
            });
        }
        Err(Error::NotFound) => {}
        Err(e) => return Err(e),
    }
    mkdir_retrying(&Dest::plain(&*provider, path.clone()), &ctx.cancel).await?;
    let node = identity_of(&*provider, &path, &observer).await;
    observer
        .on_mutation(&Mutation::Created { path: &path, node }, &ctx.actor)
        .await?;
    ctx.progress.update(|p| p.entries_done = 1);
    Ok(())
}

/// Creates an EMPTY file (#290). The same as [`mkdir_task`] with the other
/// node class, and with its same rules.
///
/// **This `stat` does NOT provide exclusivity: the provider does.**
/// [`Provider::write`](norte_vfs::Provider::write) contracts create-new, and
/// each implementation honors it with the strength its transport allows —
/// the local one with an atomic `rename_noreplace` on commit, the object one
/// with an `If-None-Match`, `MemProvider` revalidating under its lock. The
/// one real TOCTOU window is SFTP's, and it is SFTP's: v3 has no atomic
/// rename.
///
/// The `stat` here is the same thing [`mkdir_task`] does and for the same
/// reason: giving a clean, EARLY `Conflict` —before creating the staging—
/// and not claiming a node that was already there as ours. Removing it would
/// not open a hole; it would only move the error later and uglier.
pub(crate) async fn create_task(
    provider: Arc<dyn Provider>,
    path: VPath,
    dest_anchor: Option<norte_proto::DirAnchor>,
    observer: Arc<dyn MutationObserver>,
    ctx: &TaskCtx,
) -> Result<(), Error> {
    if ctx.cancel.is_cancelled() {
        return Err(Error::Cancelled);
    }
    let observer = crate::observer::pin_for_task(observer).await?;
    ctx.progress.update(|p| {
        p.entries_total = Some(1);
        p.current = Some(path.clone());
    });
    // The anchor BEFORE the `stat`: if the directory is no longer the one
    // the human listed, there is nothing to check inside it.
    anchor_parent_or_fail(
        &*provider,
        &path,
        dest_anchor.as_ref(),
        ctx.progress.snapshot().task_id.get(),
        &ctx.cancel,
    )
    .await?;
    match with_retry(&ctx.cancel, || provider.stat(&path).boxed()).await {
        Ok(_) => {
            return Err(Error::Conflict {
                conflict: ConflictKind::Exists,
            });
        }
        Err(Error::NotFound) => {}
        Err(e) => return Err(e),
    }
    // Open and close: the sink publishes its empty staging, which is exactly
    // a zero-byte file at the destination. With no `write` in between at all.
    let sink = provider.write(&path).await?;
    sink.commit().await?;
    let node = identity_of(&*provider, &path, &observer).await;
    observer
        .on_mutation(&Mutation::Created { path: &path, node }, &ctx.actor)
        .await?;
    ctx.progress.update(|p| p.entries_done = 1);
    Ok(())
}

/// What to do if `write_task`'s destination already exists (ADR 0101).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnExists {
    /// `Conflict{Exists}`: nothing is touched.
    Refuse,
    /// What was there goes to the logical trash BEFORE creating the new one:
    /// two journal entries in a row (`trashed`, `created`), and the previous
    /// content with a way back. Never overwritten in place.
    Replace,
}

/// Writes a file with CONTENT from memory (ADR 0101): what a hook asks for
/// as a sidecar. It is [`create_task`] with bytes and with an explicit
/// "already exists" policy; it carries no anchor because it does not come
/// from a listing.
#[tracing::instrument(skip_all, fields(path = %path.display_lossy(), ?on_exists, bytes = content.len()))]
pub(crate) async fn write_task(
    provider: Arc<dyn Provider>,
    path: VPath,
    content: Vec<u8>,
    on_exists: OnExists,
    observer: Arc<dyn MutationObserver>,
    ctx: &TaskCtx,
) -> Result<(), Error> {
    if ctx.cancel.is_cancelled() {
        return Err(Error::Cancelled);
    }
    let observer = crate::observer::pin_for_task(observer).await?;
    ctx.progress.update(|p| {
        p.entries_total = Some(1);
        p.current = Some(path.clone());
    });
    let mut buried: Option<Option<VPath>> = None;
    match with_retry(&ctx.cancel, || provider.stat(&path).boxed()).await {
        // Only ONE FILE is replaced. A directory, a link, or a device with
        // the approved name are not "the previous sidecar": the badge says
        // "can write a file named X", not "can bury your `.git`".
        Ok(entry) if on_exists == OnExists::Refuse || entry.kind != EntryKind::File => {
            return Err(Error::Conflict {
                conflict: ConflictKind::Exists,
            });
        }
        Ok(_) => {
            // The same burial as `delete_task`, the same way back if the row
            // does not arrive (#160).
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
            let trash_id =
                norte_vfs::trash::TrashId::new(now_ms, ctx.progress.snapshot().task_id.get());
            let dest = trash_retrying(&*provider, &path, &trash_id, &ctx.cancel).await?;
            if let Err(e) = observer
                .on_mutation(
                    &Mutation::Trashed {
                        path: &path,
                        dest: dest.as_ref(),
                    },
                    &ctx.actor,
                )
                .await
            {
                let returned = match dest.as_ref() {
                    Some(en) => provider.restore_from(en, &path).await,
                    None => Err(Error::Unsupported),
                };
                tracing::error!(
                    error = %e,
                    buried = %crate::engine::span_path(&path),
                    returned = returned.is_ok(),
                    "sidecar: the previous one was buried and its journal entry did NOT arrive",
                );
                return Err(e);
            }
            buried = Some(dest);
        }
        Err(Error::NotFound) => {}
        Err(e) => return Err(e),
    }
    // If the new one does not make it to publication, the previous one comes
    // back from the trash: a "replace" that fails halfway cannot leave the
    // directory with neither. The journal already has the `trashed` row; the
    // return is recorded as what it is, so the chain tells the whole story.
    let written = write_new(&*provider, &path, content).await;
    if let Err(e) = written {
        if let Some(Some(en)) = &buried {
            match provider.restore_from(en, &path).await {
                Ok(()) => {
                    let node = identity_of(&*provider, &path, &observer).await;
                    let _ = observer
                        .on_mutation(&Mutation::Created { path: &path, node }, &ctx.actor)
                        .await;
                }
                Err(r) => tracing::error!(
                    error = %r,
                    "sidecar: the new one was not written and the previous one did not come back from the trash"
                ),
            }
        }
        return Err(e);
    }
    let node = identity_of(&*provider, &path, &observer).await;
    observer
        .on_mutation(&Mutation::Created { path: &path, node }, &ctx.actor)
        .await?;
    ctx.progress.update(|p| p.entries_done = 1);
    Ok(())
}

/// Opens, writes, and publishes `content` at `path`; the staging is aborted
/// if the write fails.
async fn write_new(provider: &dyn Provider, path: &VPath, content: Vec<u8>) -> Result<(), Error> {
    let mut sink = provider.write(path).await?;
    if let Err(e) = sink.write(bytes::Bytes::from(content)).await {
        let _ = sink.abort().await;
        return Err(e);
    }
    sink.commit().await
}

/// Changes a batch of paths' POSIX permissions (#314).
///
/// **One journal entry PER PATH**, with the previous mode as the reversal,
/// and it is recorded BEFORE moving to the next: a batch that fails halfway
/// has to leave undone what it already did, and one entry per batch would
/// not say which ones.
///
/// The previous mode is READ before writing the new one. If it cannot be
/// read, the mutation is recorded anyway and as irreversible: changing the
/// permissions without being able to say what they were is what really
/// happens, and staying silent about it or aborting would be the two ways
/// of lying about it.
///
/// **A path that fails does not bring down the batch**: it is counted as
/// unreadable in progress and the rest are changed. The typical case is a
/// selection with a file owned by someone else inside it, and losing the
/// other fifty because of it would be punishing whoever marked correctly.
///
/// Cancellation is checked PER PATH (rule 3): a `chmod` cannot be split.
/// Expands a recursive `set_mode`'s roots to the list of nodes that are
/// going to be touched, and says how many were left UNVISITED by the cap
/// (#315).
///
/// The order is top-down —the root before its content— and that matters:
/// removing a directory's execute bit before walking it would leave the
/// rest of the tree unreachable mid-operation. With `dir_mode` at `755` it
/// does not happen; with the same mode for everything, it does, and it is
/// the footgun the documentation names. Even so it is walked whole BEFORE
/// touching anything, so that tree is changed complete and what is left
/// unreachable is whatever comes after, not this batch's own.
///
/// A SYMLINK is not followed: it is added as a node and `set_mode` skips it
/// with its reason. Following it would leave the tree the human pointed at,
/// which is what ADR 0072 has been saying all along.
///
/// A directory that cannot be listed does not kill the operation: it is
/// counted as not visited, as `dir_size` does. Dying on leaf 40,000's
/// `EACCES` would return nothing in exchange for all the work already done.
async fn expand_tree(
    roots: Vec<(Arc<dyn Provider>, VPath)>,
    ctx: &TaskCtx,
) -> Result<(Vec<(Arc<dyn Provider>, VPath)>, u64), Error> {
    let cap = usize::try_from(norte_proto::methods::SET_MODE_RECURSIVE_MAX).unwrap_or(usize::MAX);
    let mut output: Vec<(Arc<dyn Provider>, VPath)> = Vec::new();
    let mut unvisited: u64 = 0;
    let mut pending: std::collections::VecDeque<(Arc<dyn Provider>, VPath)> =
        roots.into_iter().collect();
    while let Some((provider, path)) = pending.pop_front() {
        if ctx.cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        if output.len() >= cap {
            // What is left in the queue, ALL AT ONCE, and it is cut off.
            // Counting them one by one while popping would give the queue's
            // depth at the moment of the cut and not what is left of the
            // tree; and continuing to pop just to count walks a list that
            // is not going to expand anymore.
            unvisited = unvisited
                .saturating_add(u64::try_from(pending.len().saturating_add(1)).unwrap_or(u64::MAX));
            break;
        }
        // A `stat` that fails is NOT touched blindly (#315): without it,
        // whether it is a link is unknown, and `chmod(2)` follows links —
        // a file that may be outside the tree the human pointed at would be
        // changed. It is counted as not visited and continues.
        let Ok(entry) = with_retry(&ctx.cancel, || provider.stat(&path).boxed()).await else {
            unvisited = unvisited.saturating_add(1);
            continue;
        };
        let dir = entry.kind == EntryKind::Dir;
        output.push((Arc::clone(&provider), path.clone()));
        if !dir {
            continue;
        }
        match provider.list(&path).await {
            Ok(mut stream) => {
                while let Some(item) = stream.next().await {
                    match item {
                        Ok(e) => pending.push_back((Arc::clone(&provider), e.path)),
                        // An unreadable listing entry does not bring down
                        // the walk; it is counted and continues.
                        Err(_) => unvisited = unvisited.saturating_add(1),
                    }
                }
            }
            Err(Error::Cancelled) => return Err(Error::Cancelled),
            Err(_) => unvisited = unvisited.saturating_add(1),
        }
    }
    Ok((output, unvisited))
}

pub(crate) struct SetModeOptions {
    /// The twelve bits for what is not a directory.
    pub(crate) mode: u32,
    /// Descend into the requested directories (#315).
    pub(crate) recursive: bool,
    /// The mode for DIRECTORIES. `None` = the same as files', which is what
    /// `chmod -R` does and what leaves a tree with no execute bit where one
    /// was needed.
    pub(crate) dir_mode: Option<u32>,
    /// The batch journal entries are grouped under when this is recursive
    /// (#315). `None` = a lone change, or a journal that cannot give batch
    /// ids.
    pub(crate) batch: Option<i64>,
}

pub(crate) async fn set_mode(
    paths: Vec<(Arc<dyn Provider>, VPath)>,
    opts: SetModeOptions,
    observer: Arc<dyn MutationObserver>,
    ctx: &TaskCtx,
) -> Result<(), Error> {
    let observer = crate::observer::pin_for_task(observer).await?;
    // With recursive, what is requested is the ROOT and not the list: it is
    // expanded before starting so progress can say how many there really
    // are. Expanding on the fly would leave a total that climbs while the
    // reader is looking at it, and the cap's cutoff could not be stated
    // until the end.
    let (paths, unvisited) = if opts.recursive {
        expand_tree(paths, ctx).await?
    } else {
        (paths, 0)
    };
    let total = u64::try_from(paths.len()).unwrap_or(u64::MAX);
    ctx.progress.update(|p| {
        p.entries_total = Some(total);
        p.entries_done = 0;
    });
    if unvisited > 0 {
        // The cap is stated BEFORE touching anything, and in its OWN field:
        // mixing it with `unreadable` made the frontend say "could not be
        // changed (a link, or not yours)" about nodes that were never even
        // looked at.
        ctx.progress.update(|p| p.unvisited = Some(unvisited));
        tracing::warn!(
            unvisited,
            cap = norte_proto::methods::SET_MODE_RECURSIVE_MAX,
            "fs.set_mode recursive: the tree exceeds the node cap"
        );
    }
    let mut done: u64 = 0;
    let mut failed: u64 = 0;
    for (provider, path) in paths {
        if ctx.cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        ctx.progress.update(|p| p.current = Some(path.clone()));
        // A SYMLINK is not touched, and this is not squeamishness:
        // `chmod(2)` FOLLOWS the link while this same function's `stat`
        // does NOT follow it (lstat, the trait's contract). So the mode
        // that would be saved as a reversal would be the LINK's —always
        // `0o777` on Linux— and undoing it would leave the TARGET open to
        // everyone. And there is something worse than the reversal: the
        // target can be outside the scope someone approved, so a chmod on
        // a link is a write that leaves its root. It is counted as failed,
        // and the frontend says so.
        let entry = provider.stat(&path).await;
        if matches!(&entry, Ok(e) if e.kind == EntryKind::Symlink) {
            failed = failed.saturating_add(1);
            done = done.saturating_add(1);
            ctx.progress.update(|p| {
                p.entries_done = done;
                p.unreadable = Some(failed);
            });
            continue;
        }
        // A DIRECTORY's mode can be different (#315): `chmod -R 644` on a
        // tree leaves it unusable —with no execute bit there is no
        // entering— and `dir_mode` is the explicit way out of that. Without
        // it, the same for everything.
        let is_dir = matches!(&entry, Ok(e) if e.kind == EntryKind::Dir);
        let mode = if is_dir {
            opts.dir_mode.unwrap_or(opts.mode)
        } else {
            opts.mode
        };
        let previous = crate::undo::modo_actual(provider.as_ref(), &path).await;
        match provider.set_mode(&path, mode).await {
            Ok(()) => {
                // The mode that was LEFT, re-read: `chmod(2)` silently
                // clears setgid when the caller does not belong to the
                // file's group, and a journal that said `2755` over a real
                // `755` would lie in the dangerous direction. If it cannot
                // be re-read, what was requested is noted, which is the
                // only thing known.
                let ended_up = crate::undo::modo_actual(provider.as_ref(), &path)
                    .await
                    .unwrap_or(mode);
                observer
                    .on_mutation(
                        &Mutation::ModeChanged {
                            path: &path,
                            from: previous,
                            to: ended_up,
                            batch: opts.batch,
                        },
                        &ctx.actor,
                    )
                    .await?;
            }
            Err(Error::Cancelled) => return Err(Error::Cancelled),
            Err(_) => failed = failed.saturating_add(1),
        }
        done = done.saturating_add(1);
        ctx.progress.update(|p| {
            p.entries_done = done;
            // #251: a `Completed` over a batch where half could not be
            // changed reads like a trusted total if progress does not say so.
            p.unreadable = Some(failed);
        });
    }
    Ok(())
}

/// How much `roots` take up, counting what can be read (#139).
///
/// The total is NOT returned: it travels in progress
/// (`bytes_done`/`entries_done`), which already exists and every frontend
/// knows how to paint. The last snapshot IS the result, and that is why this
/// function does not invent a new type.
///
/// **An unreadable directory does not kill the count.** Counting a large
/// tree can take minutes, and dying on leaf 40,000's `EACCES` would return
/// nothing in exchange for all the work already done: what comes out is the
/// size of what could be read. Cancellation DOES stop it: it is an order,
/// not a stumble.
///
/// It does not materialize the tree —unlike [`walk`], which returns a
/// `Vec`— because no entry is needed here after adding it up, and a
/// ten-million-file directory does not fit twice in memory for no reason.
pub(crate) async fn dir_size(
    roots: Vec<(std::sync::Arc<dyn Provider>, VPath)>,
    ctx: &TaskCtx,
) -> Result<(), Error> {
    let mut bytes: u64 = 0;
    let mut entries: u64 = 0;
    let mut unreadables: u64 = 0;
    for (provider, root) in roots {
        // The root counts by itself: measuring a lone FILE is a legitimate
        // question and walks nothing.
        match provider.stat(&root).await {
            Ok(e) if e.kind != EntryKind::Dir => {
                bytes = bytes.saturating_add(e.size.unwrap_or(0));
                entries = entries.saturating_add(1);
                ctx.progress.update(|p| {
                    p.bytes_done = bytes;
                    p.entries_done = entries;
                    p.unreadable = Some(unreadables);
                });
                continue;
            }
            Ok(_) => {}
            // A root that cannot even be looked at counts as unreadable and
            // does not bring down the count of the others: a selection of
            // twenty folders is not lost over one.
            Err(e) => {
                if matches!(e, Error::Cancelled) {
                    return Err(Error::Cancelled);
                }
                unreadables = unreadables.saturating_add(1);
                continue;
            }
        }
        let mut pending = vec![root];
        while let Some(dir) = pending.pop() {
            if ctx.cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let mut stream = match provider.list(&dir).await {
                Ok(s) => s,
                Err(e) => {
                    if matches!(e, Error::Cancelled) {
                        return Err(Error::Cancelled);
                    }
                    unreadables = unreadables.saturating_add(1);
                    continue;
                }
            };
            ctx.progress.update(|p| p.current = Some(dir.clone()));
            while let Some(item) = stream.next().await {
                // A real inner loop (rule 3): a dir with 10^6 entries or a
                // slow provider cannot delay cancellation until the pop.
                if ctx.cancel.is_cancelled() {
                    return Err(Error::Cancelled);
                }
                let entry = match item {
                    Ok(e) => e,
                    Err(e) => {
                        if matches!(e, Error::Cancelled) {
                            return Err(Error::Cancelled);
                        }
                        unreadables = unreadables.saturating_add(1);
                        continue;
                    }
                };
                entries = entries.saturating_add(1);
                if entry.kind == EntryKind::Dir {
                    pending.push(entry.path);
                } else {
                    // A LAZY listing carries no sizes (#52: the local
                    // provider leaves them at `None` and whoever needs them
                    // asks for them), so here they have to be requested:
                    // summing `unwrap_or(0)` gave "0 B" for a whole tree,
                    // which is the most wrong answer possible to the only
                    // question that was asked.
                    //
                    // A `stat` per file is what `du` does, and what a
                    // copy's hydration does (`hydrate_plan`). On a provider
                    // that DOES carry the size in the listing, nothing is
                    // requested. In series, like hydration: against SFTP
                    // that is N trips and it is already noted as #156 for
                    // both.
                    let size = match entry.size {
                        Some(n) => Some(n),
                        None => match provider.stat(&entry.path).await {
                            Ok(st) => st.size,
                            Err(Error::Cancelled) => return Err(Error::Cancelled),
                            // A file that can be listed but not stated
                            // counts as unreadable: its size is unknown, and
                            // the rest of the count is not lost over it.
                            Err(_) => {
                                unreadables = unreadables.saturating_add(1);
                                None
                            }
                        },
                    };
                    bytes = bytes.saturating_add(size.unwrap_or(0));
                }
                ctx.progress.update(|p| {
                    p.bytes_done = bytes;
                    p.entries_done = entries;
                    p.unreadable = Some(unreadables);
                });
            }
        }
    }
    // The number TRAVELS (#251), it does not stay in a log. `fs.dir_size` exists for
    if unreadables > 0 {
        tracing::info!(
            unreadables,
            "fs.dir_size: parts of the tree could not be read"
        );
    }
    ctx.progress.update(|p| {
        p.bytes_done = bytes;
        p.entries_done = entries;
        // On finishing, the total IS what was counted: stating it closes
        // the bar instead of leaving it at an "out of how much" that never
        // arrived.
        p.bytes_total = Some(bytes);
        p.entries_total = Some(entries);
        // And the number TRAVELS (#251), it does not stay in the log above:
        // this method exists to answer "does this fit at the destination?",
        // and a tree where half gave `EACCES` reported `Completed` with a
        // trusted total that was too small. With this, whoever paints it
        // says "at least X". Always `Some`: `fs.dir_size` DOES count
        // unreadables, and `Some(0)` is an answer —"I counted them and
        // there were none"— that `None` cannot give.
        p.unreadable = Some(unreadables);
        p.current = None;
    });
    Ok(())
}

/// What `root` is made of, child by child (`fs.dir_usage`, 0.75.0, phase 4).
///
/// A sibling of [`dir_size`] with the question flipped: that one answers
/// "how much does this take up?" in ONE number, and this one answers "what
/// is it made of?" with an accumulator per child. That is why the total is
/// not enough and there is a report: a map is painted with the list, not
/// with the sum.
///
/// **The root's listing is all or nothing.** An error mid-stream PROPAGATES
/// —same as in [`list_base_names`], and for the same reason: a half listing
/// produces a half map, and a map missing the 400 GB child is
/// indistinguishable from one where that child does not exist. What IS
/// tolerated is a child that cannot be MEASURED: it comes out with `partial`
/// and the others are measured.
///
/// **`partial` goes PER CHILD** (not per report) because that is what
/// allows painting: the incomplete rectangle is marked and the rest of the
/// map stays true.
///
/// # It is measured WHILE it is listed, and that is why it materializes nothing
/// A `/nix/store` —the case [`DIR_USAGE_MAX_CHILDREN`] exists to
/// accommodate— has hundreds of thousands of first-level children. Saving
/// the whole listing to measure it afterward bounds the WIRE and leaves the
/// daemon's heap unbounded: every retained child is a `Segment` and a
/// `VPath` with all its stretches. Measuring on the fly keeps nothing but
/// the top-N alive, so memory is O(cap) and not O(children).
///
/// The consequence shows up in the report: while the listing runs, `listed`
/// is `false` —nobody knows yet how many children there are— and `pending`
/// is zero because no listed child is waiting to be measured. When `listed`
/// turns `true`, everything is measured. That is exactly what `listed`
/// exists to say.
///
/// # Above the cap the LARGEST survive
/// The protocol promises it, and it is the only thing that is useful: a map
/// that paints the first 4096 children alphabetically and sends the 400 GB
/// one to `omitted` is the function not existing. It is pruned on the fly
/// —when one bigger than the smallest retained comes in, the smallest
/// leaves and `omitted` climbs—, so the partial report also keeps the
/// promise, and they are emitted in LISTING order, which is the contract's
/// other end.
///
/// The tiebreak is by NAME bytes, not by folded text: it has to give the
/// same result on ext4, on NTFS, and on APFS, which sort their listings
/// three different ways.
pub(crate) async fn dir_usage(
    provider: std::sync::Arc<dyn Provider>,
    root: VPath,
    report: std::sync::Arc<std::sync::Mutex<norte_proto::methods::FsDirUsageReportResult>>,
    ctx: &TaskCtx,
) -> Result<(), Error> {
    use norte_proto::methods::{DIR_USAGE_MAX_CHILDREN, DirUsageChild};

    // What a FILE is made of is not a question: it is made of itself. It is
    // rejected here and not answered with a single-rectangle map, which is
    // the answer that looks useful and is not.
    match provider.stat(&root).await {
        Ok(e) if e.kind == EntryKind::Dir => {}
        Ok(_) => return Err(Error::InvalidPath),
        Err(e) => return Err(e),
    }

    // Where NOT to descend. Comes from the SAME place as `fs.search`,
    // `fs.compare`, and `archive.pack`'s exclusions: the read gate looks at
    // the request's ROOT and nothing else (#165), so a map of `$HOME`
    // requested by an agent would drag the daemon's state directory along
    // with it — and a size and an entry count over `journal.db` and
    // `secrets.age`, queryable in a loop, are a side channel on what the
    // human does.
    let excluded = crate::policy::walk_exclusions(&ctx.actor);

    let mut bytes: u64 = 0;
    let mut entries: u64 = 0;
    let mut done: u64 = 0;
    let mut unreadables: u64 = 0;
    let mut omitted_count: u64 = 0;

    let mut stream = provider.list(&root).await?;
    while let Some(item) = stream.next().await {
        // A real inner loop (rule 3): a directory with 10^6 entries cannot
        // delay cancellation until the end of the listing.
        if ctx.cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let entry = item?;
        // An entry with no base name is the root itself, which some object
        // providers return in their own listing; it is not a child. It is
        // not counted anywhere, on purpose: it has no name to paint nor
        // subtree to measure.
        let Some(name) = entry.path.file_name().cloned() else {
            continue;
        };
        // Defense in depth, as in `search::run_walk`: the measurement's
        // scope is a CORE invariant, not the provider behaving well. A
        // remote server that lists a path outside the requested directory
        // would hang another location's bytes off a name from here.
        if !crate::policy::is_under(&root, &entry.path) {
            continue;
        }
        if excluded
            .iter()
            .any(|x| crate::policy::is_under(x, &entry.path))
        {
            continue;
        }
        ctx.progress
            .update(|p| p.current = Some(entry.path.clone()));
        let (child_bytes, child_entries, child_unreadable) = if entry.kind == EntryKind::Dir {
            measure_subtree(provider.as_ref(), &entry.path, &excluded, ctx, bytes).await?
        } else {
            // A LAZY listing carries no sizes (#52), so here they have to be
            // requested — same reason and same cost as in `dir_size`.
            match entry.size {
                Some(n) => (n, 1, 0),
                None => match provider.stat(&entry.path).await {
                    Ok(st) => (st.size.unwrap_or(0), 1, u64::from(st.size.is_none())),
                    Err(Error::Cancelled) => return Err(Error::Cancelled),
                    // Can be listed but not stated: counts as an entry, its
                    // size is a lower bound, and it says so.
                    Err(_) => (0, 1, 1),
                },
            }
        };
        bytes = bytes.saturating_add(child_bytes);
        entries = entries.saturating_add(child_entries);
        done = done.saturating_add(1);
        unreadables = unreadables.saturating_add(child_unreadable);
        let child = DirUsageChild {
            name,
            kind: entry.kind,
            bytes: child_bytes,
            entries: child_entries,
            partial: child_unreadable > 0,
        };
        {
            let mut r = report
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            // With the list full, this child leaves ONE with no name: either
            // it, or the smallest it displaces. In both cases `omitted`
            // climbs by one.
            if r.children.len() >= DIR_USAGE_MAX_CHILDREN {
                omitted_count = omitted_count.saturating_add(1);
            }
            retain_largest(&mut r.children, child, DIR_USAGE_MAX_CHILDREN);
            r.total_bytes = bytes;
            r.total_entries = entries;
            r.omitted = omitted_count;
        }
        ctx.progress.update(|p| {
            p.bytes_done = bytes;
            p.entries_done = done;
            // #251: a `Completed` over a map where several children are
            // lower bounds reads like a trusted total if progress does not
            // say so. The report says it per child; this says it on the board.
            p.unreadable = Some(unreadables);
        });
    }

    // The listing finished: NOW it is known how many children there were,
    // and none is left half done because they were measured as they arrived.
    {
        let mut r = report
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        r.listed = true;
        r.pending = 0;
    }
    // A provider that ADMITS having left entries out (an archive's index
    // with names it could not represent) cannot produce a map that says
    // "this is all". It does not go to `omitted` —which promises that what
    // was omitted's bytes ARE in the totals, and this one's are not— but to
    // `unvisited`, which is exactly "the tree is bigger than what I walked".
    let skipped_hint = provider.list_skipped(&root).await.ok().flatten();
    ctx.progress.update(|p| {
        p.bytes_total = Some(bytes);
        p.entries_total = Some(done);
        p.unvisited = skipped_hint;
        p.current = None;
    });
    Ok(())
}

/// Puts `child` into `children` if it is one of the LARGEST, evicting the
/// smallest.
///
/// Below `cap` everything gets in. From there on it competes: the
/// candidate displaces the smallest retained one, or it is left out. This
/// way the report keeps the protocol's promise —"above the cap the largest
/// travel"— also WHILE it runs, which is when it can be asked for.
///
/// **The listing order is kept without sorting anything**: children arrive
/// in that order and are appended at the end, and removing one from the
/// middle does not change the order of the ones that remain.
///
/// The comparison key is `(bytes, name bytes)`. The tiebreak by RAW NAME,
/// and not by folded or normalized text, because the result has to be the
/// same on ext4, on NTFS, and on APFS: each sorts its listing its own way,
/// and which child keeps its name cannot depend on that.
fn retain_largest(
    children: &mut Vec<norte_proto::methods::DirUsageChild>,
    child: norte_proto::methods::DirUsageChild,
    cap: usize,
) {
    if children.len() < cap {
        children.push(child);
        return;
    }
    let victim = children
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| {
            a.bytes
                .cmp(&b.bytes)
                .then_with(|| a.name.as_bytes().cmp(b.name.as_bytes()))
        })
        .map(|(i, c)| (i, c.bytes, c.name.as_bytes().to_vec()));
    if let Some((i, vb, vn)) = victim
        && (child.bytes, child.name.as_bytes()) > (vb, vn.as_slice())
    {
        children.remove(i);
        children.push(child);
    }
}

/// How much `dir`'s subtree takes up, counting it too: `(bytes, entries,
/// unreadables)`.
///
/// `unreadables` is HOW MANY things under here could not be read, and it is
/// what turns the other two numbers into a declared LOWER BOUND. An
/// `EACCES` on leaf 40,000 cannot throw out the whole map — but it cannot be
/// silenced either, which is what a count that only returns the number would do.
///
/// It is a COUNT and not a `bool` because the same counter travels in
/// `TaskProgress::unreadable`, which in `dir_size` counts unreadable things:
/// a field that meant "how many" in one task and "some" in its sibling could
/// not be read by anyone. The child's `partial` is derived from here (`> 0`).
///
/// `base_bytes` is what the PREVIOUS children have added up, so progress is
/// published from inside the real loop: without this, a single 400 GB child
/// leaves the bar frozen for minutes, and a bar stopped in a cancelable task
/// is indistinguishable from a hung one.
///
/// **The only `Err` is [`Error::Cancelled`]**; any other failure is folded
/// into the unreadable count. Whoever calls it depends on that `?`: if this
/// ever returned another error, one unreadable child would bring down the
/// whole map.
///
/// Cancellation DOES stop it: it is an order, not a stumble.
async fn measure_subtree(
    provider: &dyn Provider,
    dir: &VPath,
    excluded: &[VPath],
    ctx: &TaskCtx,
    base_bytes: u64,
) -> Result<(u64, u64, u64), Error> {
    let mut bytes: u64 = 0;
    // It counts as an entry itself: `entries` is "what is inside, itself
    // included", which is what makes the children add up to the parent's total.
    let mut entries: u64 = 1;
    let mut unreadables: u64 = 0;
    let mut pending = vec![dir.clone()];
    while let Some(current) = pending.pop() {
        if ctx.cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let mut stream = match provider.list(&current).await {
            Ok(s) => s,
            Err(Error::Cancelled) => return Err(Error::Cancelled),
            Err(_) => {
                unreadables = unreadables.saturating_add(1);
                continue;
            }
        };
        while let Some(item) = stream.next().await {
            if ctx.cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let entry = match item {
                Ok(e) => e,
                Err(Error::Cancelled) => return Err(Error::Cancelled),
                Err(_) => {
                    unreadables = unreadables.saturating_add(1);
                    continue;
                }
            };
            // The same exclusions as above, and here for the same reason:
            // the gate looked at the root, and below it there can be a
            // directory this actor does not descend into.
            if excluded
                .iter()
                .any(|x| crate::policy::is_under(x, &entry.path))
            {
                continue;
            }
            entries = entries.saturating_add(1);
            if entry.kind == EntryKind::Dir {
                pending.push(entry.path);
            } else {
                let size = match entry.size {
                    Some(n) => Some(n),
                    None => match provider.stat(&entry.path).await {
                        Ok(st) => st.size,
                        Err(Error::Cancelled) => return Err(Error::Cancelled),
                        Err(_) => None,
                    },
                };
                if size.is_none() {
                    unreadables = unreadables.saturating_add(1);
                }
                bytes = bytes.saturating_add(size.unwrap_or(0));
            }
            // From the INNER loop: that is where time is spent.
            let seen = base_bytes.saturating_add(bytes);
            ctx.progress.update(|p| p.bytes_done = seen);
        }
    }
    Ok((bytes, entries, unreadables))
}

/// Walks the tree under `root` (not including it). Order guarantee: every
/// directory appears BEFORE any of its descendants.
pub(crate) async fn walk(
    provider: &dyn Provider,
    root: &VPath,
    cancel: &CancellationToken,
) -> Result<Vec<Entry>, Error> {
    let mut out = Vec::new();
    let mut pending = vec![root.clone()];
    while let Some(dir) = pending.pop() {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let mut stream = provider.list(&dir).await?;
        while let Some(item) = stream.next().await {
            // A real inner loop (rule 3): a dir with 10^6 entries or a slow
            // provider cannot delay cancellation until the pop.
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let entry = item?;
            if entry.kind == EntryKind::Dir {
                pending.push(entry.path.clone());
            }
            out.push(entry);
        }
    }
    Ok(out)
}

/// A copy plan entry: provenance decides how a move's DELETE treats it (the
/// copy ignores it — issue #19).
#[derive(Debug)]
struct PlanEntry {
    entry: Entry,
    provenance: Provenance,
}

/// A plan entry's origin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Provenance {
    /// A real dirent of the source tree: deleted in a move.
    Real,
    /// A REAL dir-symlink expanded by Follow: in a move THE LINK is deleted
    /// (a single remove), never its content.
    LinkRoot,
    /// Seen THROUGH an expanded link: belongs to the link's TARGET; a move
    /// never deletes it (`LinkRoot` takes the link with it).
    ViaLink,
}

/// What does a symlink point to?
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TargetKind {
    /// File — or broken: the Follow leaf will give its honest error when copying.
    File,
    /// Directory.
    Dir,
}

/// Probes a symlink's target type WITHOUT opening the node: `list()`
/// validates with metadata that follows the link (Ok = dir; `TypeMismatch` =
/// file or other; `NotFound` = broken — the Follow leaf will give its honest
/// error when copying). Never `read()`: opening a symlink→FIFO would block
/// the blocking thread without honoring cancellation (rust-reviewer's M3
/// finding). Dropping the stream cancels the listing.
async fn probe_symlink_target(
    provider: &dyn Provider,
    p: &VPath,
    cancel: &CancellationToken,
) -> Result<TargetKind, Error> {
    match with_retry(cancel, || provider.list(p).boxed()).await {
        Ok(probe) => {
            drop(probe);
            Ok(TargetKind::Dir)
        }
        // TypeMismatch = file/other; NotFound = broken → in both cases the
        // Follow leaf decides (and will give its honest error if it applies).
        Err(
            Error::Conflict {
                conflict: ConflictKind::TypeMismatch,
            }
            | Error::NotFound,
        ) => Ok(TargetKind::File),
        Err(e) => Err(e),
    }
}

/// A tree's plan: a flat walk (everything `Real`) except under Follow, where
/// dir-symlinks are expanded with cycle detection (issue #19).
async fn plan_for(
    provider: &dyn Provider,
    root: &VPath,
    opts: TransferOptions,
    cancel: &CancellationToken,
) -> Result<Vec<PlanEntry>, Error> {
    if opts.symlinks == SymlinkPolicy::Follow {
        walk_following(provider, root, false, cancel).await
    } else {
        Ok(walk(provider, root, cancel)
            .await?
            .into_iter()
            .map(|entry| PlanEntry {
                entry,
                provenance: Provenance::Real,
            })
            .collect())
    }
}

/// A directory pending the Follow walk: its path, the identity chain of its
/// ancestors (cycles, spec §17.9), and whether it was reached through an
/// expanded link.
struct DirFrame {
    dir: VPath,
    ancestors: Vec<NodeId>,
    via_link: bool,
}

/// Walk with dir-symlink expansion (`SymlinkPolicy::Follow`, issue #19):
/// every symlink is probed; the ones pointing to a dir become synthetic dirs
/// and it descends THROUGH the link. A link whose resolved target is already
/// in the ancestor chain is a CYCLE → [`Error::Loop`]. Expanding requires
/// identity ([`Provider::node_id`]): without it, `Unsupported` — exactly
/// M1's behavior (trees with no dir-symlinks do not need it and keep
/// working).
///
/// `root_is_link` = the root itself is a dir-symlink to expand (all its
/// content ends up `ViaLink` and the move deletes only the root link).
async fn walk_following(
    provider: &dyn Provider,
    root: &VPath,
    root_is_link: bool,
    cancel: &CancellationToken,
) -> Result<Vec<PlanEntry>, Error> {
    // The root's identity opens the ancestor chain. For a link root it is
    // MANDATORY (expanding without a visited set would be Russian roulette);
    // for a normal dir, best-effort (with no ids it will only fail if a
    // dir-symlink to expand shows up).
    let root_id =
        match with_retry(cancel, || provider.node_id(root, FollowLinks::Yes).boxed()).await? {
            Some(id) => Some(id),
            None if root_is_link => return Err(Error::Unsupported),
            None => None,
        };
    let mut out: Vec<PlanEntry> = Vec::new();
    let mut pending = vec![DirFrame {
        dir: root.clone(),
        ancestors: root_id.into_iter().collect(),
        via_link: root_is_link,
    }];
    while let Some(frame) = pending.pop() {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let mut stream = provider.list(&frame.dir).await?;
        while let Some(item) = stream.next().await {
            // A real inner loop (rule 3), as in walk().
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let entry = item?;
            let provenance = if frame.via_link {
                Provenance::ViaLink
            } else {
                Provenance::Real
            };
            match entry.kind {
                EntryKind::Dir => {
                    let id = with_retry(cancel, || {
                        provider.node_id(&entry.path, FollowLinks::Yes).boxed()
                    })
                    .await?;
                    let mut ancestors = frame.ancestors.clone();
                    ancestors.extend(id);
                    pending.push(DirFrame {
                        dir: entry.path.clone(),
                        ancestors,
                        via_link: frame.via_link,
                    });
                    out.push(PlanEntry { entry, provenance });
                }
                EntryKind::Symlink => {
                    match probe_symlink_target(provider, &entry.path, cancel).await? {
                        TargetKind::File => out.push(PlanEntry { entry, provenance }),
                        TargetKind::Dir => {
                            let Some(id) = with_retry(cancel, || {
                                provider.node_id(&entry.path, FollowLinks::Yes).boxed()
                            })
                            .await?
                            else {
                                return Err(Error::Unsupported);
                            };
                            if frame.ancestors.contains(&id) {
                                // Cycle: following it would copy forever. Its
                                // own category since 0.4.0 (#31, ADR 0011).
                                return Err(Error::Loop);
                            }
                            let mut ancestors = frame.ancestors.clone();
                            ancestors.push(id);
                            pending.push(DirFrame {
                                dir: entry.path.clone(),
                                ancestors,
                                via_link: true,
                            });
                            // SYNTHETIC dir: the copy creates a real dir at
                            // the destination. The reference mtime comes
                            // from the link's `entry` exactly as the
                            // listing gave it — with a lazy local listing
                            // (#52) it is almost always `None` today
                            // (`hydrate_plan` does not touch Dirs); no
                            // consumer reads it yet.
                            out.push(PlanEntry {
                                entry: Entry {
                                    // attrs: SYNTHETIC dir, empty on purpose
                                    // (block 2 decision from #108): a
                                    // `PlanEntry` is internal to the core —
                                    // the attributes are listing
                                    // presentation and are NOT propagated
                                    // from the consumed link (they would
                                    // describe the LINK, not the synthetic
                                    // dir replacing it).
                                    attrs: std::collections::BTreeMap::new(),
                                    path: entry.path,
                                    kind: EntryKind::Dir,
                                    size: None,
                                    mtime_ms: entry.mtime_ms,
                                },
                                provenance: if frame.via_link {
                                    Provenance::ViaLink
                                } else {
                                    Provenance::LinkRoot
                                },
                            });
                        }
                    }
                }
                EntryKind::File | EntryKind::Other => out.push(PlanEntry { entry, provenance }),
            }
        }
    }
    Ok(out)
}

/// Relocates `path` (a descendant of `from`) under `to`, segment by segment.
fn rebase(path: &VPath, from: &VPath, to: &VPath) -> Result<VPath, Error> {
    let prefix_len = from.segments().count();
    let mut target = to.clone();
    for seg in path.segments().skip(prefix_len) {
        // Invariant: the segments come from an already validated VPath.
        let seg = Segment::new(seg.to_vec()).map_err(|_| Error::Internal { panic: false })?;
        target = target.join(seg);
    }
    Ok(target)
}

/// `true` if `child` is a PROPER descendant of `ancestor` (same scheme and
/// authority, strict segment prefix), BYTE FOR BYTE.
///
/// Valid for comparing two paths that came from the SAME listing snapshot —
/// where the bytes are already what the provider gave— and NOT valid for
/// deciding whether a transfer falls inside its own source: that is asked
/// against the DESTINATION volume with [`is_descendant_folded`] (#269).
fn is_descendant(child: &VPath, ancestor: &VPath) -> bool {
    if child.scheme() != ancestor.scheme() || child.authority() != ancestor.authority() {
        return false;
    }
    let a: Vec<&[u8]> = ancestor.segments().collect();
    let c: Vec<&[u8]> = child.segments().collect();
    c.len() > a.len() && c[..a.len()] == a[..]
}

/// The same question, folding the segments under the DESTINATION volume's
/// mode (#269) — the usual domain gotcha: case and normalization are decided
/// by the destination, not the source.
///
/// The scenario that opened it: pane A at `/home` on `docs`, pane B
/// navigated to `/home/DOCS`, which on APFS or NTFS **is** `/home/docs`.
/// Then `to = /home/DOCS/docs` and the byte comparison said no, while
/// `same_node` exited early because the segment count differs. Neither
/// guard tripped and the tree was copied inside itself: bounded —the plan is
/// a snapshot— but it is exactly what the guard exists to prevent, and the
/// nested `docs/docs` is a trap for whoever cleans up afterward.
///
/// Under `FoldMode::None` it is literally [`is_descendant`]: with no
/// folding, the byte comparison is already the answer.
async fn is_descendant_folded(child: &VPath, ancestor: &VPath, dst: &dyn Provider) -> bool {
    if child.scheme() != ancestor.scheme() || child.authority() != ancestor.authority() {
        return false;
    }
    let a: Vec<&[u8]> = ancestor.segments().collect();
    let c: Vec<&[u8]> = child.segments().collect();
    if c.len() <= a.len() {
        return false;
    }
    let mode = fold_mode_at(child, dst).await;
    if mode == norte_encoding::FoldMode::None {
        return c[..a.len()] == a[..];
    }
    a.iter().zip(&c).all(|(x, y)| {
        x == y || norte_encoding::name_key(x, mode) == norte_encoding::name_key(y, mode)
    })
}

/// The digest of each path's CONTENT, in the order they were requested
/// (`fs.checksum`, 0.59.0, #311).
///
/// The report fills in AS it is computed, not at the end: whoever asks for
/// it while it runs sees what it has, which is what makes checking a
/// hundred files without waiting for all hundred useful. `pending` is what
/// is left, and reaches zero with the last one.
///
/// **Cancelling leaves `pending > 0` on an already terminal Task**, and that
/// is not an oversight: it is the signal that the report is half done.
/// Whoever reads it has to look at the Task's state besides the number — a
/// reader that only polled `pending == 0` would never stop, and one that
/// treated the truncated report as final would say "missing" about files
/// nobody ever looked at.
///
/// **An unreadable file does not kill the batch**, same as in [`dir_size`]:
/// it comes out with its reason and the rest are computed. A DIRECTORY is
/// not walked — it comes out marked, because hashing a tree is a different
/// question with its own format.
///
/// **Cancellation is checked per CHUNK**, not per file (rule 3): checking it
/// per file would let cancelling in the middle of a 40 GB one wait for it to
/// finish reading, which is exactly when someone cancels.
///
/// It does not materialize the content: it is read in whatever chunks the
/// provider gives and only one lives at a time, so a 40 GB file costs 40 GB
/// of reading and not of memory.
pub(crate) async fn checksum(
    paths: Vec<(std::sync::Arc<dyn Provider>, VPath)>,
    report: std::sync::Arc<std::sync::Mutex<norte_proto::methods::FsChecksumReportResult>>,
    ctx: &TaskCtx,
) -> Result<(), Error> {
    use norte_proto::methods::{ChecksumEntry, ChecksumMiss};

    let total = u64::try_from(paths.len()).unwrap_or(u64::MAX);
    {
        let mut r = report
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        r.pending = total;
    }
    ctx.progress.update(|p| {
        p.entries_total = Some(total);
        p.entries_done = 0;
    });
    let mut bytes: u64 = 0;
    let mut done: u64 = 0;
    let mut unreadables: u64 = 0;
    for (provider, path) in paths {
        if ctx.cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        ctx.progress.update(|p| p.current = Some(path.clone()));
        // What is not a file is not read: a directory here is a selection
        // that included a folder, not an error from whoever requested it.
        let entry = match provider.stat(&path).await {
            Ok(e) if e.kind == EntryKind::Dir => ChecksumEntry {
                path: path.clone(),
                digest: None,
                miss: Some(ChecksumMiss::NotAFile),
            },
            Ok(_) => match digest_of(provider.as_ref(), &path, &ctx.cancel).await {
                Ok(d) => {
                    bytes = bytes.saturating_add(d.1);
                    ChecksumEntry {
                        path: path.clone(),
                        digest: Some(d.0),
                        miss: None,
                    }
                }
                Err(Error::Cancelled) => return Err(Error::Cancelled),
                Err(_) => ChecksumEntry {
                    path: path.clone(),
                    digest: None,
                    miss: Some(ChecksumMiss::Unreadable),
                },
            },
            Err(Error::Cancelled) => return Err(Error::Cancelled),
            Err(_) => ChecksumEntry {
                path: path.clone(),
                digest: None,
                miss: Some(ChecksumMiss::Unreadable),
            },
        };
        done = done.saturating_add(1);
        if entry.digest.is_none() {
            unreadables = unreadables.saturating_add(1);
        }
        {
            let mut r = report
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            r.entries.push(entry);
            r.pending = total.saturating_sub(done);
        }
        ctx.progress.update(|p| {
            p.entries_done = done;
            p.bytes_done = bytes;
            // #251: a `Completed` with half the batch missing a digest reads
            // like a trusted total if progress does not say so. The report
            // says it in full, but whoever only watches the board sees this.
            p.unreadable = Some(unreadables);
        });
    }
    Ok(())
}

/// `path`'s sha256 in lowercase hex, and how many bytes were read.
///
/// Always LOWERCASE hex, like the rest of the protocol's digests: two writes
/// of the same hash that compare differently are a bug waiting to happen.
async fn digest_of(
    provider: &dyn Provider,
    path: &VPath,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<(String, u64), Error> {
    use futures::StreamExt as _;
    use sha2::{Digest as _, Sha256};

    let mut stream = provider.read(path, None).await?;
    let mut hasher = Sha256::new();
    let mut read_count: u64 = 0;
    while let Some(chunk) = stream.next().await {
        // By CHUNK and not by file (rule 3).
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let chunk = chunk?;
        read_count = read_count.saturating_add(chunk.len() as u64);
        hasher.update(&chunk);
    }
    Ok((
        norte_proto::hashing::hex_lower(&hasher.finalize()),
        read_count,
    ))
}

/// How the volume containing `at` folds names, asked of the DESTINATION
/// provider. `capabilities_at` answers for the MOUNT (#215): a FAT thumb
/// drive under a case-sensitive `/home` does not inherit `/home`'s answer.
async fn fold_mode_at(at: &VPath, dst: &dyn Provider) -> norte_encoding::FoldMode {
    let caps = dst
        .capabilities_at(at)
        .await
        .unwrap_or_else(|_| dst.capabilities());
    norte_compare::Sides::mode_of(caps)
}

#[cfg(test)]
mod tests {
    use norte_proto::{Entry, Error};

    use super::{is_descendant_folded, rename_auto_candidate, same_node_heuristic};

    /// #269 — the "inside itself" guard compared prefixes BYTE FOR BYTE, and
    /// on a folding volume (APFS, NTFS, exFAT, an SMB/SFTP against a folding
    /// server) `/home/DOCS` **is** `/home/docs`. With the destination pane
    /// navigated there, `from = /home/docs` and `to = /home/DOCS/docs`: the
    /// guard did not trip, `same_node` exited early because the segment
    /// count differs, and the core copied a tree inside itself.
    #[tokio::test]
    async fn inside_itself_folds_under_the_destinations_mode() {
        use norte_proto::{CapabilityFlags, VPath};
        use norte_testkit::MemProvider;

        let vp = |w: &str| VPath::parse(w).expect("wire");
        let from = vp("mem:///home/docs");
        let to = vp("mem:///home/DOCS/docs");

        // Volume that DISTINGUISHES case: they are two different trees, and
        // copying one inside the other is a legitimate operation.
        let sensitive = MemProvider::new();
        assert!(!is_descendant_folded(&to, &from, &sensitive).await);

        // Volume that FOLDS: it is the same tree.
        let folds = MemProvider::with_flags(
            CapabilityFlags::RENAME_ATOMIC | CapabilityFlags::CASE_PRESERVING,
        );
        assert!(
            is_descendant_folded(&to, &from, &folds).await,
            "a tree is copied inside itself"
        );

        // And it is still a STRICT prefix: the same folded path is not a
        // descendant of itself (that is `same_node`'s job).
        assert!(!is_descendant_folded(&vp("mem:///home/DOCS"), &from, &folds).await);

        // Nor a sibling whose name only shares a BYTE prefix.
        assert!(!is_descendant_folded(&vp("mem:///home/docsx/y"), &from, &folds).await);
    }

    /// #215: the identity heuristic asks about the LOCATION, not the
    /// provider, and folds with the shared key and not with `to_lowercase`.
    ///
    /// `capabilities()` answers for the provider's mount, so under the same
    /// `file://` a case-insensitive thumb drive used to get `/home`'s
    /// answer. What is decided with it is whether a `move` is a rename onto
    /// itself — i.e. a copy path that can destroy the source.
    #[tokio::test]
    async fn the_identity_heuristic_asks_about_the_location() {
        use norte_proto::{CapabilityFlags, VPath};

        /// Case-sensitive everywhere EXCEPT under `/thumb`.
        struct ByMount(norte_testkit::MemProvider);

        #[async_trait::async_trait]
        impl norte_vfs::Provider for ByMount {
            fn scheme(&self) -> &str {
                self.0.scheme()
            }
            fn capabilities(&self) -> norte_proto::Capabilities {
                let mut c = self.0.capabilities();
                c.flags.insert(CapabilityFlags::CASE_SENSITIVE);
                c
            }
            async fn capabilities_at(&self, p: &VPath) -> Result<norte_proto::Capabilities, Error> {
                let mut c = self.capabilities();
                if p.segments().any(|s| s == b"thumb") {
                    c.flags.remove(CapabilityFlags::CASE_SENSITIVE);
                }
                Ok(c)
            }
            async fn stat(&self, p: &VPath) -> Result<Entry, Error> {
                self.0.stat(p).await
            }
            async fn list(&self, p: &VPath) -> Result<norte_vfs::EntryStream, Error> {
                self.0.list(p).await
            }
            async fn read(
                &self,
                p: &VPath,
                r: Option<norte_proto::ByteRange>,
            ) -> Result<norte_vfs::ByteStream, Error> {
                self.0.read(p, r).await
            }
            async fn write(&self, p: &VPath) -> Result<Box<dyn norte_vfs::ByteSink>, Error> {
                self.0.write(p).await
            }
            async fn mkdir(&self, p: &VPath) -> Result<(), Error> {
                self.0.mkdir(p).await
            }
            async fn remove(&self, p: &VPath) -> Result<(), Error> {
                self.0.remove(p).await
            }
            async fn rename(&self, a: &VPath, b: &VPath) -> Result<(), Error> {
                self.0.rename(a, b).await
            }
        }

        let dst = ByMount(norte_testkit::MemProvider::new());
        let vp = |w: &str| VPath::parse(w).expect("wire");

        // Under the root, which distinguishes case: two names, not one.
        assert!(
            !same_node_heuristic(&vp("mem:///home/A.txt"), &vp("mem:///home/a.txt"), &dst).await
        );
        // Under the mount that does NOT distinguish it: the same file.
        assert!(
            same_node_heuristic(&vp("mem:///thumb/A.txt"), &vp("mem:///thumb/a.txt"), &dst).await,
            "the mount decides, not the provider"
        );
        // And it folds with the shared key: the micro sign and the Greek mu
        // are the same name under folding, and `to_lowercase` does not move them.
        assert!(
            same_node_heuristic(&vp("mem:///thumb/%C2%B5"), &vp("mem:///thumb/%CE%BC"), &dst).await,
            "the folding key, not a to_lowercase"
        );
    }

    /// Clean cancellation of `trash_retrying` (rule 3): an already cancelled
    /// token returns `Cancelled` WITHOUT touching the provider (the
    /// nonexistent victim is not even consulted → no `NotFound`), and the
    /// retry loop checks the token on every lap.
    #[tokio::test]
    async fn trash_retrying_honors_the_cancelled_token() {
        use norte_proto::{Error, VPath};
        use norte_testkit::MemProvider;
        use norte_vfs::trash::TrashId;
        use tokio_util::sync::CancellationToken;

        let mem = MemProvider::new().with_logical_trash();
        let cancel = CancellationToken::new();
        cancel.cancel();
        let id = TrashId::new(0, 0);
        let r = super::trash_retrying(&mem, &VPath::parse("mem:///x").expect("wire"), &id, &cancel)
            .await;
        assert!(matches!(r, Err(Error::Cancelled)), "{r:?}");
    }

    /// #104 review MAJOR-1: `mkdir_retrying`'s ambiguous `Conflict` is
    /// VERIFIED by listing — someone else's dir WITH CONTENT is never
    /// claimed as ours (a false `Created` would make an undo send it whole
    /// to the trash). The residual window (someone else's dir still EMPTY)
    /// is accepted and pinned as a decision: recoverable from trash,
    /// indistinguishable without a node-id.
    #[tokio::test]
    async fn mkdir_retrying_does_not_claim_someone_elsis_dir_with_content() {
        use norte_proto::{Error, VPath};
        use norte_testkit::MemProvider;
        use norte_vfs::Provider as _;
        use tokio_util::sync::CancellationToken;

        let mem = MemProvider::new();
        let dir = VPath::parse("mem:///x").expect("wire");
        // A third party: dir WITH content, already present when the retry arrives.
        mem.mkdir(&dir).await.expect("someone else's mkdir");
        {
            let mut s = mem
                .write(&VPath::parse("mem:///x/theirs.txt").expect("wire"))
                .await
                .expect("write");
            s.write(bytes::Bytes::from_static(b"foreign"))
                .await
                .expect("chunk");
            s.commit().await.expect("commit");
        }
        // First attempt: transient WITHOUT applying → ambiguous.
        mem.faults().unavailable_for_next(1);
        let cancel = CancellationToken::new();
        let r = super::mkdir_retrying(&super::Dest::plain(&mem, dir.clone()), &cancel).await;
        assert!(
            matches!(r, Err(Error::Conflict { .. })),
            "a dir with content belongs to a third party, never ours: {r:?}"
        );

        // Pinned decision: someone else's EMPTY dir does pass as ours
        // (residual window documented in `mkdir_retrying`'s rustdoc).
        let empty = VPath::parse("mem:///empty").expect("wire");
        mem.mkdir(&empty).await.expect("someone else's empty mkdir");
        mem.faults().unavailable_for_next(1);
        let r = super::mkdir_retrying(&super::Dest::plain(&mem, empty.clone()), &cancel).await;
        assert!(r.is_ok(), "{r:?}");
    }

    /// Clean cancellation of `mkdir_task` (rule 3, #104): an already
    /// cancelled token returns `Cancelled` BEFORE touching the provider —
    /// neither pre-stat, nor mkdir, nor `Created` to the journal (the
    /// observer would record the mutation; an unapplied mkdir must never
    /// reach it).
    #[tokio::test]
    async fn mkdir_task_honors_the_cancelled_token() {
        use crate::journal::Actor;
        use crate::progress::ProgressReporter;
        use crate::scheduler::TaskCtx;
        use norte_proto::{Error, TaskId, TaskKind, VPath};
        use norte_testkit::MemProvider;
        use norte_vfs::Provider as _;
        use std::sync::Arc;
        use tokio_util::sync::CancellationToken;

        let mem = Arc::new(MemProvider::new());
        let cancel = CancellationToken::new();
        cancel.cancel();
        let (reporter, _rx) = ProgressReporter::new(TaskId::new(1), TaskKind::Mkdir);
        let ctx = TaskCtx {
            pause: crate::scheduler::PauseGate::default(),
            cancel,
            progress: Arc::new(reporter),
            actor: Actor::User,
        };
        let observer: Arc<dyn crate::MutationObserver> = Arc::new(crate::observer::NoopObserver);
        let r = super::mkdir_task(
            mem.clone(),
            VPath::parse("mem:///x").expect("wire"),
            observer,
            &ctx,
        )
        .await;
        assert!(matches!(r, Err(Error::Cancelled)), "{r:?}");
        assert!(
            (*mem)
                .stat(&VPath::parse("mem:///x").expect("wire"))
                .await
                .is_err(),
            "nothing created under cancellation"
        );
    }

    /// Hard rule 3: every new task has its clean cancellation test.
    ///
    /// `create_task` checks the token ONCE, on entry, and that is enough
    /// because it has no loop: what comes after is a `stat`, a `write`, and
    /// a `commit`, and splitting the creation of an empty file in half means
    /// nothing. What this test nails down is that under cancellation NO
    /// half-done file is left at the destination.
    #[tokio::test]
    async fn create_task_honors_the_cancelled_token() {
        use crate::journal::Actor;
        use crate::progress::ProgressReporter;
        use crate::scheduler::TaskCtx;
        use norte_proto::{Error, TaskId, TaskKind, VPath};
        use norte_testkit::MemProvider;
        use norte_vfs::Provider as _;
        use std::sync::Arc;
        use tokio_util::sync::CancellationToken;

        let mem = Arc::new(MemProvider::new());
        let cancel = CancellationToken::new();
        cancel.cancel();
        let (reporter, _rx) = ProgressReporter::new(TaskId::new(1), TaskKind::Create);
        let ctx = TaskCtx {
            pause: crate::scheduler::PauseGate::default(),
            cancel,
            progress: Arc::new(reporter),
            actor: Actor::User,
        };
        let observer: Arc<dyn crate::MutationObserver> = Arc::new(crate::observer::NoopObserver);
        let r = super::create_task(
            mem.clone(),
            VPath::parse("mem:///new.txt").expect("wire"),
            None,
            observer,
            &ctx,
        )
        .await;
        assert!(matches!(r, Err(Error::Cancelled)), "{r:?}");
        assert!(
            (*mem)
                .stat(&VPath::parse("mem:///new.txt").expect("wire"))
                .await
                .is_err(),
            "nothing created under cancellation, not even empty"
        );
    }

    /// Rule 3: `write_task` honors the cancelled token before touching
    /// anything, and a destination that is a DIRECTORY is not replaced (ADR
    /// 0101).
    #[tokio::test]
    async fn write_task_honors_the_cancelled_token_and_does_not_replace_a_directory() {
        use crate::journal::Actor;
        use crate::progress::ProgressReporter;
        use crate::scheduler::TaskCtx;
        use norte_proto::{Error, TaskId, TaskKind, VPath};
        use norte_testkit::MemProvider;
        use norte_vfs::Provider as _;
        use std::sync::Arc;
        use tokio_util::sync::CancellationToken;

        let mem = Arc::new(MemProvider::new());
        let cancel = CancellationToken::new();
        cancel.cancel();
        let (reporter, _rx) = ProgressReporter::new(TaskId::new(1), TaskKind::Create);
        let ctx = TaskCtx {
            pause: crate::scheduler::PauseGate::default(),
            cancel,
            progress: Arc::new(reporter),
            actor: Actor::Plugin {
                id: "org.x.y".into(),
            },
        };
        let observer: Arc<dyn crate::MutationObserver> = Arc::new(crate::observer::NoopObserver);
        let p = VPath::parse("mem:///d/.log").expect("wire");
        let r = super::write_task(
            mem.clone(),
            p.clone(),
            b"x".to_vec(),
            super::OnExists::Replace,
            Arc::clone(&observer),
            &ctx,
        )
        .await;
        assert!(matches!(r, Err(Error::Cancelled)), "{r:?}");
        assert!(
            (*mem).stat(&p).await.is_err(),
            "nothing created under cancellation"
        );

        // A directory with the sidecar's name: `Conflict`, and it stays there.
        let (reporter, _rx) = ProgressReporter::new(TaskId::new(2), TaskKind::Create);
        let ctx = TaskCtx {
            pause: crate::scheduler::PauseGate::default(),
            cancel: CancellationToken::new(),
            progress: Arc::new(reporter),
            actor: Actor::Plugin {
                id: "org.x.y".into(),
            },
        };
        let dir = VPath::parse("mem:///dir").expect("wire");
        (*mem).mkdir(&dir).await.expect("mkdir");
        let r = super::write_task(
            mem.clone(),
            dir.clone(),
            b"x".to_vec(),
            super::OnExists::Replace,
            observer,
            &ctx,
        )
        .await;
        assert!(matches!(r, Err(Error::Conflict { .. })), "{r:?}");
        assert!(
            (*mem).stat(&dir).await.is_ok(),
            "the directory is still there"
        );
    }

    /// An observer that says no to EVERYTHING: what the next two test is the
    /// path where the mutation already happened and its record does not
    /// arrive — exactly #160's weapon in `delete_task`.
    #[derive(Debug)]
    struct FailingObserver;

    #[async_trait::async_trait]
    impl crate::MutationObserver for FailingObserver {
        async fn on_mutation(
            &self,
            _mutation: &crate::Mutation<'_>,
            _actor: &crate::journal::Actor,
        ) -> Result<(), norte_proto::Error> {
            Err(norte_proto::Error::Io { retryable: false })
        }
    }

    /// ADR 0147 and hard rule 3, the riskiest path: a COPY paused halfway
    /// —with the staging file open— and cancelled during the wait leaves the
    /// destination CLEAN (with no `resume`): neither the final name nor an
    /// unmarked partial.
    #[tokio::test]
    async fn a_copy_paused_and_cancelled_leaves_the_destination_clean() {
        use crate::journal::Actor;
        use crate::progress::ProgressReporter;
        use crate::scheduler::{PauseGate, TaskCtx};
        use futures::StreamExt as _;
        use norte_proto::{Error, TaskId, TaskKind, TaskState, VPath};
        use norte_testkit::MemProvider;
        use norte_vfs::Provider as _;
        use std::sync::Arc;
        use tokio_util::sync::CancellationToken;

        let mem = Arc::new(MemProvider::new());
        let source = VPath::parse("mem:///o/grande.bin").expect("wire");
        let dest_dir = VPath::parse("mem:///d").expect("wire");
        let dest = VPath::parse("mem:///d/grande.bin").expect("wire");
        mem.mkdir(&VPath::parse("mem:///o").expect("wire"))
            .await
            .expect("mkdir");
        mem.mkdir(&dest_dir).await.expect("mkdir");
        {
            let mut sink = mem.write(&source).await.expect("write");
            sink.write(bytes::Bytes::from(vec![7u8; 200_000]))
                .await
                .expect("chunk");
            sink.commit().await.expect("commit");
        }
        let (reporter, mut rx) = ProgressReporter::new(TaskId::new(1), TaskKind::Copy);
        let cancel = CancellationToken::new();
        let pause = PauseGate::default();
        pause.pause();
        let ctx = TaskCtx {
            pause,
            cancel: cancel.clone(),
            progress: Arc::new(reporter),
            actor: Actor::User,
        };
        let provider = Arc::clone(&mem) as Arc<dyn norte_vfs::Provider>;
        let copy = super::copy_task(
            Arc::clone(&provider),
            provider,
            source,
            dest.clone(),
            crate::engine::TransferOptions::default(),
            None,
            Arc::new(crate::observer::NoopObserver),
            &ctx,
        );
        let watcher = async {
            tokio::time::timeout(
                std::time::Duration::from_secs(10),
                rx.wait_for(|p| p.state == TaskState::Paused),
            )
            .await
            .expect("reaches pause")
            .expect("live sender");
            cancel.cancel();
        };
        let (r, ()) = tokio::join!(copy, watcher);
        assert!(matches!(r, Err(Error::Cancelled)), "{r:?}");
        assert!(mem.stat(&dest).await.is_err(), "no final name");
        let remaining: Vec<_> = mem
            .list(&dest_dir)
            .await
            .expect("list")
            .collect::<Vec<_>>()
            .await;
        assert!(
            remaining.is_empty(),
            "neither staging nor partial: {remaining:?}"
        );
    }

    /// ADR 0147 and hard rule 3: a PAUSED recursive delete touches nothing
    /// while it waits, says so (`Paused`), and cancelling it during the wait
    /// finishes clean with the whole tree in place.
    #[tokio::test]
    async fn a_paused_delete_waits_and_cancelling_it_touches_nothing() {
        use crate::journal::Actor;
        use crate::progress::ProgressReporter;
        use crate::scheduler::{PauseGate, TaskCtx};
        use norte_proto::{DeleteMode, Error, TaskId, TaskKind, TaskState, VPath};
        use norte_testkit::MemProvider;
        use norte_vfs::Provider as _;
        use std::sync::Arc;
        use tokio_util::sync::CancellationToken;

        let mem = Arc::new(MemProvider::new());
        let dir = VPath::parse("mem:///d").expect("wire");
        mem.mkdir(&dir).await.expect("mkdir");
        for n in ["a", "b", "c"] {
            let f = VPath::parse(&format!("mem:///d/{n}")).expect("wire");
            let mut sink = mem.write(&f).await.expect("write");
            sink.write(bytes::Bytes::from_static(b"x"))
                .await
                .expect("chunk");
            sink.commit().await.expect("commit");
        }
        let (reporter, mut rx) = ProgressReporter::new(TaskId::new(1), TaskKind::Delete);
        let cancel = CancellationToken::new();
        let pause = PauseGate::default();
        pause.pause();
        let ctx = TaskCtx {
            pause,
            cancel: cancel.clone(),
            progress: Arc::new(reporter),
            actor: Actor::User,
        };
        let delete = super::delete_task(
            Arc::clone(&mem) as Arc<dyn norte_vfs::Provider>,
            dir.clone(),
            DeleteMode::Permanent,
            Arc::new(crate::observer::NoopObserver),
            &ctx,
        );
        let watcher = async {
            tokio::time::timeout(
                std::time::Duration::from_secs(10),
                rx.wait_for(|p| p.state == TaskState::Paused),
            )
            .await
            .expect("reaches pause")
            .expect("live sender");
            for n in ["a", "b", "c"] {
                let f = VPath::parse(&format!("mem:///d/{n}")).expect("wire");
                assert!(mem.stat(&f).await.is_ok(), "paused does not delete {n}");
            }
            cancel.cancel();
        };
        let (r, ()) = tokio::join!(delete, watcher);
        assert!(matches!(r, Err(Error::Cancelled)), "{r:?}");
        assert!(mem.stat(&dir).await.is_ok(), "the tree is still whole");
    }

    /// #160, the same shape as in `sync::exec::bury` and via the path an F8
    /// walks: the trash took the file and the journal's observer failed
    /// afterward. It is returned, and the delete fails with the tree as it was.
    #[tokio::test]
    async fn an_observer_that_fails_after_burying_returns_the_file() {
        use crate::journal::Actor;
        use crate::progress::ProgressReporter;
        use crate::scheduler::TaskCtx;
        use norte_proto::{DeleteMode, Error, TaskId, TaskKind, VPath};
        use norte_testkit::MemProvider;
        use norte_vfs::Provider as _;
        use std::sync::Arc;
        use tokio_util::sync::CancellationToken;

        let mem = Arc::new(MemProvider::new().with_logical_trash());
        let path = VPath::parse("mem:///a.txt").expect("wire");
        {
            let mut sink = mem.write(&path).await.expect("write");
            sink.write(bytes::Bytes::from_static(b"alive"))
                .await
                .expect("chunk");
            sink.commit().await.expect("commit");
        }

        // The same `TaskCtx` built by hand as this module's neighboring
        // tests (there is no shared helper; none is added for one more test).
        let (reporter, _rx) = ProgressReporter::new(TaskId::new(1), TaskKind::Delete);
        let ctx = TaskCtx {
            pause: crate::scheduler::PauseGate::default(),
            cancel: CancellationToken::new(),
            progress: Arc::new(reporter),
            actor: Actor::User,
        };

        let err = super::delete_task(
            Arc::clone(&mem) as Arc<dyn norte_vfs::Provider>,
            path.clone(),
            DeleteMode::Trash,
            Arc::new(FailingObserver),
            &ctx,
        )
        .await
        .expect_err("the observer failed");
        assert!(matches!(err, Error::Io { .. }), "{err:?}");

        assert!(
            mem.stat(&path).await.is_ok(),
            "the file came back from the trash to its path"
        );
    }

    /// #160, the other arm: a "vanish" trash (macOS/Windows, `Opaque`) does
    /// not name what it took — `dest` arrives `None` and there is nowhere
    /// for `restore_from` to point. Compensation is not possible; all that
    /// is left for the operator is the log line, and the observer's failure
    /// keeps propagating so the delete fails loudly (success is not pretended).
    #[tokio::test]
    async fn an_observer_that_fails_with_an_opaque_trash_does_not_compensate_but_still_fails_loudly()
     {
        use crate::journal::Actor;
        use crate::progress::ProgressReporter;
        use crate::scheduler::TaskCtx;
        use norte_proto::{DeleteMode, Error, TaskId, TaskKind, VPath};
        use norte_testkit::MemProvider;
        use norte_vfs::Provider as _;
        use std::sync::Arc;
        use tokio_util::sync::CancellationToken;

        // Without `.with_logical_trash()`: the testkit's trash is "vanish"
        // (like macOS/Windows' native one) and `trash()` returns `Ok(None)`.
        let mem = Arc::new(MemProvider::new());
        let path = VPath::parse("mem:///a.txt").expect("wire");
        {
            let mut sink = mem.write(&path).await.expect("write");
            sink.write(bytes::Bytes::from_static(b"alive"))
                .await
                .expect("chunk");
            sink.commit().await.expect("commit");
        }

        let (reporter, _rx) = ProgressReporter::new(TaskId::new(1), TaskKind::Delete);
        let ctx = TaskCtx {
            pause: crate::scheduler::PauseGate::default(),
            cancel: CancellationToken::new(),
            progress: Arc::new(reporter),
            actor: Actor::User,
        };

        let err = super::delete_task(
            Arc::clone(&mem) as Arc<dyn norte_vfs::Provider>,
            path.clone(),
            DeleteMode::Trash,
            Arc::new(FailingObserver),
            &ctx,
        )
        .await
        .expect_err("the observer failed, same as with a named trash");
        assert!(matches!(err, Error::Io { .. }), "{err:?}");

        assert!(
            mem.stat(&path).await.is_err(),
            "with no `dest` there is no possible compensation: the file is \
             still out of place, and that is said by the log, not by a \
             `stat` that finds it again"
        );
    }

    /// NEGATIVE branch of symlink disambiguation (encoding-auditor, fixture
    /// 3): after a transient, the Conflict with a FOREIGN link (different
    /// target) stays Conflict and the foreign link is left intact. And the
    /// positive branch disambiguates just the same with non-UTF8 bytes.
    #[tokio::test]
    async fn symlink_retrying_does_not_adopt_foreign_links_and_disambiguates_raw_bytes() {
        use futures::StreamExt as _;
        use norte_proto::Error;
        use norte_testkit::MemProvider;
        use norte_vfs::{Provider, SymlinkKind};
        use tokio_util::sync::CancellationToken;

        let mem = MemProvider::new();
        let root = MemProvider::root();
        let seg = |b: &[u8]| norte_proto::Segment::new(b.to_vec()).expect("valid segment");
        let cancel = CancellationToken::new();

        // Negative: preexisting link with ANOTHER target + prior transient.
        let foreign = root.join(seg(b"foreign"));
        mem.symlink(&foreign, b"other", SymlinkKind::File)
            .await
            .expect("previous symlink");
        mem.faults().unavailable_for_next(1);
        let res = super::symlink_retrying(
            &super::Dest::plain(&mem, foreign.clone()),
            b"ours",
            SymlinkKind::File,
            &cancel,
        )
        .await;
        assert!(
            matches!(res, Err(Error::Conflict { .. })),
            "a foreign link is never adopted: {res:?}"
        );
        assert_eq!(
            mem.read_link(&foreign).await.expect("intact"),
            b"other",
            "the foreign link is not touched"
        );

        // Positive with RAW non-UTF8 bytes (rule 1: byte comparison).
        let raw = root.join(seg(b"raw"));
        mem.faults().ambiguous_mutations(1);
        super::symlink_retrying(
            &super::Dest::plain(&mem, raw.clone()),
            b"caf\xE9",
            SymlinkKind::File,
            &cancel,
        )
        .await
        .expect("effect applied + verified by bytes = ok");
        assert_eq!(mem.read_link(&raw).await.expect("exists"), b"caf\xE9");
        // The list stream is still alive after all this (sanity).
        drop(mem.list(&root).await.expect("list ok").next().await);
    }

    #[test]
    fn rename_auto_respects_the_extension() {
        assert_eq!(rename_auto_candidate(b"a.txt", 1), b"a (1).txt");
        assert_eq!(rename_auto_candidate(b"a.txt", 12), b"a (12).txt");
        assert_eq!(rename_auto_candidate(b"no-ext", 1), b"no-ext (1)");
        // Dotfile: the leading dot is NOT an extension.
        assert_eq!(rename_auto_candidate(b".bashrc", 1), b".bashrc (1)");
        // Only the LAST extension (documented limitation: tar.gz gets split).
        assert_eq!(rename_auto_candidate(b"file.tar.gz", 1), b"file.tar (1).gz");
        // Byte-safe with non-UTF8 names.
        assert_eq!(
            rename_auto_candidate(&[0xE9, b'.', b'd'], 2),
            &[0xE9, b' ', b'(', b'2', b')', b'.', b'd'][..]
        );
    }
}
