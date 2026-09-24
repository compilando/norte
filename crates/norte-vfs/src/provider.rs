//! The [`Provider`] trait: the single contract every storage backend has.
//!
//! Contract rules (`provider_contract!` in `norte-testkit` verifies them):
//! - Names = bytes ([`VPath`]); a provider never silently renormalizes or
//!   "repairs" names.
//! - Simple operations: no recursion (the core does it), no collision
//!   policies (the core decides), never follows symlinks.
//! - Errors mapped to the [`Error`] taxonomy at the provider's boundary.

use async_trait::async_trait;
use bytes::Bytes;
use futures::stream::BoxStream;
use norte_proto::{AttrInfo, ByteRange, Capabilities, Entry, Error, Segment, VPath};

use crate::options::ListOptions;
use crate::sink::ByteSink;

/// Stream of entries from a listing (`fs.list`), lazy and cancellable by
/// dropping it. An error mid-stream ends the listing.
pub type EntryStream = BoxStream<'static, Result<Entry, Error>>;

/// Stream of a file's content, in [`Bytes`] chunks of whatever size the
/// provider prefers (the copy engine re-chunks if it needs to).
pub type ByteStream = BoxStream<'static, Result<Bytes, Error>>;

/// A storage backend (local, sftp, s3, archive, memory).
///
/// Object-safe: the core works with `Box<dyn Provider>` registered by
/// scheme. Composite operations (recursive copy, cross-provider move,
/// deleting trees) do NOT live here: they belong to `norte-core`'s copy
/// engine.
///
/// ```
/// use async_trait::async_trait;
/// use norte_proto::{Capabilities, CapabilityFlags, Entry, Error, VPath};
/// use norte_vfs::{ByteSink, ByteStream, EntryStream, Provider};
///
/// struct NullProvider;
///
/// #[async_trait]
/// impl Provider for NullProvider {
///     fn scheme(&self) -> &str {
///         "null"
///     }
///     fn capabilities(&self) -> Capabilities {
///         Capabilities { flags: CapabilityFlags::empty(), max_path: None }
///     }
///     async fn stat(&self, _p: &VPath) -> Result<Entry, Error> {
///         Err(Error::NotFound)
///     }
///     async fn list(&self, _p: &VPath) -> Result<EntryStream, Error> {
///         Err(Error::NotFound)
///     }
///     async fn read(
///         &self,
///         _p: &VPath,
///         _range: Option<norte_proto::ByteRange>,
///     ) -> Result<ByteStream, Error> {
///         Err(Error::NotFound)
///     }
///     async fn write(&self, _p: &VPath) -> Result<Box<dyn ByteSink>, Error> {
///         Err(Error::Unsupported)
///     }
///     async fn mkdir(&self, _p: &VPath) -> Result<(), Error> {
///         Err(Error::Unsupported)
///     }
///     async fn remove(&self, _p: &VPath) -> Result<(), Error> {
///         Err(Error::NotFound)
///     }
///     async fn rename(&self, _from: &VPath, _to: &VPath) -> Result<(), Error> {
///         Err(Error::Unsupported)
///     }
/// }
///
/// // Object-safe: the core registers providers this way.
/// let _boxed: Box<dyn Provider> = Box::new(NullProvider);
///
/// // `list_skipped`'s default (#93): a backend that lists everything that
/// // exists answers `Ok(None)` — nothing to signal. `Some(0)` = an indexed
/// // container with no omissions; `Some(n)` = n entries invisible to the listing.
/// let p = VPath::parse("null:///").unwrap();
/// let skipped = futures::executor::block_on(NullProvider.list_skipped(&p)).unwrap();
/// assert_eq!(skipped, None);
///
/// // Block 2's defaults (#108): empty catalogue, list_with ≡ list.
/// assert!(NullProvider.attrs().is_empty());
/// ```
// MAINTENANCE NOTE: every NEW method on this trait (even one with a
// default) must also be delegated in `SessionProvider`
// (norte-core/src/sessions.rs) — otherwise cached remote sessions would
// serve the DEFAULT instead of the live provider, with no compile error.
// There's a completeness test there.
#[async_trait]
pub trait Provider: Send + Sync {
    /// The scheme this provider serves (`file`, `sftp`, `mem`…).
    fn scheme(&self) -> &str;

    /// Declared capabilities; the core picks its strategy by consulting them.
    fn capabilities(&self) -> Capabilities;

    /// Capabilities REFINED for `p`: the same declaration, corrected with
    /// whatever the backend can find out about THAT location — how that
    /// mount folds case, whether the directory is an ext4/f2fs `+F`
    /// ([`norte_proto::CapabilityFlags::FULL_FOLD`]), whether a write
    /// under it can be confined
    /// ([`norte_proto::CapabilityFlags::CONFINED_WRITES`]).
    ///
    /// It's `async` because the answer costs I/O: a probe goes in
    /// `spawn_blocking` (hard rule 2), not on the runtime. The default
    /// answers [`Self::capabilities`], which is correct for any backend
    /// whose locations are all alike; overriding it is for one that serves
    /// more than one filesystem behind the same scheme (ADR 0054).
    ///
    /// A probe that can't answer is NOT an error: the declaration is
    /// returned. [`Capabilities`] can't say "I don't know" — an absent
    /// flag means absent — and that's a decision, not an oversight: the
    /// degradation is exactly the usual declared behavior.
    ///
    /// Errors: whatever `p` produces ([`Error::NotFound`] if it doesn't exist).
    async fn capabilities_at(&self, p: &VPath) -> Result<Capabilities, Error> {
        let _ = p;
        Ok(self.capabilities())
    }

    /// Can a name with these bytes EXIST on this backend?
    ///
    /// Pure and I/O-free: these are the filesystem's rules, not the
    /// tree's state. The default says yes to everything, which is correct
    /// for any backend that accepts any byte sequence without `/` or
    /// NUL — the case for POSIX and for most remotes.
    ///
    /// Exists for the DESTINATION of a copy or a sync (#163): nothing
    /// checked that a name legal under the source root was legal under
    /// the destination's, so `CON`, `f:ads` or a trailing dot — all legal
    /// on ext4 — used to be discovered at execution time. The worst of
    /// the four is `f:ads`: on NTFS it **works** and writes an alternate
    /// data stream, so the copy reports success and the file isn't there.
    ///
    /// Whoever implements it is whoever knows its rules, and that's why
    /// it belongs to the provider and not to a table in the core: an
    /// `sftp` to a Windows server and a `file://` on Linux don't have the
    /// same rules, and the core doesn't know what's on the other side.
    fn name_is_legal(&self, name: &[u8]) -> bool {
        let _ = name;
        true
    }

    /// Opens `root` as a CONFINED ROOT: everything done with the handle
    /// addresses segments RELATIVE to it and can't escape, whatever the
    /// shape of the tree underneath — an INTERMEDIATE component that's a
    /// symlink pointing outside fails instead of redirecting the write
    /// (#164).
    ///
    /// It isn't a check before opening: there's no path left to
    /// recompose, which is what makes it free of the TOCTOU window a
    /// caller-side check has by construction.
    ///
    /// What's forbidden is ESCAPING, not "having symlinks": one pointing
    /// somewhere else inside the root is followed, because forbidding it
    /// would break legitimate trees without gaining any security.
    ///
    /// [`Error::Unsupported`] (default) = this backend doesn't know how
    /// to confine. The caller DEGRADES — it doesn't reject — and says so;
    /// see [`norte_proto::CapabilityFlags::CONFINED_WRITES`], which
    /// announces it per location (ADR 0054).
    ///
    /// Errors: whatever opening `root` produces ([`Error::NotFound`] if
    /// it isn't there).
    ///
    /// ```
    /// # use norte_vfs::Provider;
    /// # use norte_proto::{Error, VPath};
    /// # async fn demo(p: &dyn Provider) {
    /// let root_path = VPath::parse("mem:///destination").expect("wire");
    /// match p.open_root(&root_path).await {
    ///     // The backend confines: everything done with the handle is
    ///     // relative to `root_path` and can't escape it.
    ///     Ok(root) => {
    ///         assert!(root.root_id().await.is_ok());
    ///     }
    ///     // And whoever doesn't know how says so, instead of faking it:
    ///     // the caller degrades to the by-path route and counts it.
    ///     Err(Error::Unsupported) => {}
    ///     Err(e) => panic!("open_root answered {e:?}"),
    /// }
    /// # }
    /// ```
    async fn open_root(&self, root: &VPath) -> Result<Box<dyn ConfinedRoot>, Error> {
        let _ = root;
        Err(Error::Unsupported)
    }

    /// A node's metadata. Symlinks: describes the LINK (kind `Symlink`),
    /// never the target.
    async fn stat(&self, p: &VPath) -> Result<Entry, Error>;

    /// Non-recursive listing of a directory, as a lazy stream. The order
    /// is the backend's, with no guarantee.
    async fn list(&self, p: &VPath) -> Result<EntryStream, Error>;

    /// Total entries of the CONTAINER under `p` omitted from its index
    /// (#93): names that don't map to valid `VPath` segments, or entries
    /// trimmed by anti-bomb limits (archive providers, ADR 0018 C2). It's
    /// a total PER CONTAINER — the omitted ones have no representable
    /// path to be attributed to, so the same value applies to any dir of
    /// that container.
    ///
    /// `Ok(None)` (default) = doesn't apply: this backend lists
    /// everything that exists (filesystems, remotes). `Ok(Some(0))` = an
    /// indexed container with no omissions. Frontends only signal
    /// `Some(n)` with `n > 0`.
    async fn list_skipped(&self, p: &VPath) -> Result<Option<u64>, Error> {
        let _ = p;
        Ok(None)
    }

    /// Catalogue of per-entry attributes this provider knows how to
    /// materialize (#108 block 2, ADR 0039). Default: none. A provider
    /// with a NON-empty catalogue MUST override [`Self::list_with`] and
    /// [`Self::stat_with`] — the contract suite pins down the agreement
    /// between the declared type ([`norte_proto::AttrType`]) and the
    /// values produced.
    ///
    /// The ids/labels here are provider-side; the daemon wraps them in
    /// `AttrCatalog::new` (which sanitizes) before touching the wire, and
    /// the embedded backend must do the same (ADR 0039 §4).
    fn attrs(&self) -> &[AttrInfo] {
        &[]
    }

    /// [`Self::list`] with options. The default ignores the options and
    /// produces bare entries — correct for any provider with an empty
    /// catalogue. Absence means absence: an unknown or unproducible
    /// requested id is OMITTED from `Entry::attrs`, never fabricated.
    async fn list_with(&self, p: &VPath, opt: &ListOptions) -> Result<EntryStream, Error> {
        let _ = opt;
        self.list(p).await
    }

    /// [`Self::stat`] with options. Same contract as [`Self::list_with`].
    async fn stat_with(&self, p: &VPath, opt: &ListOptions) -> Result<Entry, Error> {
        let _ = opt;
        self.stat(p).await
    }

    /// A file's content as a stream of chunks. `range: None` = the whole
    /// file; with a range, from `offset` up to `len` bytes (or EOF,
    /// whichever comes first). `offset` past EOF: empty stream, not an
    /// error (`pread` semantics). Required by M2's resume and the viewer
    /// (ADR 0005).
    async fn read(&self, p: &VPath, range: Option<ByteRange>) -> Result<ByteStream, Error>;

    /// The node's REAL identity, if the backend knows one: `(dev, ino)` on
    /// unix, `(volume, FileId)` on Windows, an internal key on synthetic
    /// providers. It's the basis of the copy engine's
    /// self-destruction guards and of the visited set against symlink
    /// cycles (spec §17.9).
    ///
    /// `follow` chooses between the identity of the node ITSELF (lstat
    /// semantics, consistent with [`Self::stat`]) or that of its resolved
    /// target; over a node that isn't a symlink both agree.
    ///
    /// `Ok(None)` (default) = this backend has no stable identity (object
    /// storage, ftp): the caller degrades to conservative heuristics and
    /// features that REQUIRE identity (following dir-symlinks) answer
    /// `Unsupported`.
    ///
    /// Errors: [`Error::NotFound`] if `p` doesn't exist — or if it's a
    /// broken symlink with [`FollowLinks::Yes`].
    async fn node_id(&self, p: &VPath, follow: FollowLinks) -> Result<Option<NodeId>, Error> {
        let _ = (p, follow);
        Ok(None)
    }

    /// RAW bytes of a symlink's target (relative or absolute, maybe
    /// broken, maybe non-UTF8 — never validated as a `VPath` nor resolved).
    ///
    /// Errors: [`Error::NotFound`] if `p` doesn't exist;
    /// [`Error::Conflict`] (`TypeMismatch`) if it exists but isn't a symlink;
    /// [`Error::Unsupported`] if the provider doesn't know about symlinks (default).
    async fn read_link(&self, p: &VPath) -> Result<Vec<u8>, Error> {
        let _ = p;
        Err(Error::Unsupported)
    }

    /// Moves `p` (the whole tree if it's a dir) to the provider's TRASH —
    /// recoverable (ADR 0009). Only with the `TRASH` capability; without
    /// it: [`Error::Unsupported`] (default) — the engine NEVER degrades
    /// to permanent deletion on its own.
    ///
    /// Returns `Some(dest)` with the recoverable destination whenever the
    /// provider CHOOSES that destination and knows how to name it — the
    /// core persists it as `reversal_ref` for undo. The LOGICAL trash
    /// (`.norte-trash/<id>/payload`) and `norte-vfs-local`'s freedesktop
    /// trash (`<Trash>/files/<name>`) do this. `None` if the trash is the
    /// OS's and doesn't expose a stable path (macOS, Windows) or is a
    /// "vanish" test trash.
    ///
    /// **`None` isn't a cosmetic detail**: without a `reversal_ref`
    /// undoing an overwrite has to match by ORIGINAL path, and by the
    /// time it gets there the most recent candidate is the file it just
    /// buried itself. That's why [`Provider::trash_restorable`] exists:
    /// a sync's plan marks IRREVERSIBLE everything that goes through a
    /// trash that doesn't name its destination, before anyone approves
    /// anything (hard rule 4).
    ///
    /// `id` is generated by the engine ONCE per operation (#99): the
    /// logical trash builds its deterministic `.norte-trash/<id>/` entry
    /// with it, so the operation is IDEMPOTENT — a retry after a
    /// transient failure converges on the same entry (victim already
    /// moved + destination present → `Some(payload)`) instead of creating
    /// a second one or losing the `reversal_ref`. Native/vanish trashes
    /// ignore it (no recoverable destination).
    ///
    /// Known platform exceptions (ADR 0009, issues #25/#26): Windows can
    /// DESTROY non-recyclable items (auto-answer to the nuke warning);
    /// freedesktop cross-device degrades to internal copy+delete
    /// (potentially long and uncancellable mid-way).
    async fn trash(&self, p: &VPath, id: &crate::trash::TrashId) -> Result<Option<VPath>, Error> {
        let _ = (p, id);
        Err(Error::Unsupported)
    }

    /// Does this provider's trash NAME the destination of what it buries?
    ///
    /// It's a property of the IMPLEMENTATION, not of a specific victim,
    /// and that's why it does no I/O: `true` means "when `trash` goes
    /// well, it answers `Some`". Whoever plans a mutation uses it to
    /// classify the reversal BEFORE asking for approval (hard rule 4):
    /// over a trash that answers `None` there's no possible undo, not
    /// even a copy's, because undoing a creation also goes through the
    /// trash (#65).
    ///
    /// A provider that answers `true` can still return `None` in a
    /// specific case — the destination exists but falls outside what that
    /// provider knows how to name —; the journal is then left without a
    /// `reversal_ref` and undo BLOCKS it by naming the path, which is the
    /// honest thing to do. What isn't legal is the opposite: promising
    /// `false` and returning `Some`, or promising `true` and never having
    /// a destination. The contract suite checks this.
    ///
    /// Default `false`: whoever doesn't implement it promises nothing.
    fn trash_restorable(&self) -> bool {
        false
    }

    /// Returns to `original` what [`Provider::trash`] buried at `dest`,
    /// with whatever metadata the trash would have left alongside it.
    ///
    /// The default is a plain move, which is what the LOGICAL trash does.
    /// It's overridden by whoever leaves metadata outside the payload —
    /// `norte-vfs-local`'s freedesktop trash also has to take along the
    /// `info/<name>.trashinfo`, or the user's trash is left with an entry
    /// pointing at a file that's no longer there.
    ///
    /// The destination has to be FREE: it inherits [`Provider::rename`]'s
    /// no-replace contract, because restoring by overwriting would lose
    /// whatever had arrived at that path afterward.
    ///
    /// # Errors
    /// Whatever [`Provider::rename`]'s: [`Error::NotFound`] if `dest` is
    /// no longer there, [`Error::Conflict`] if `original` is occupied.
    async fn restore_from(&self, dest: &VPath, original: &VPath) -> Result<(), Error> {
        self.rename(dest, original).await
    }

    /// GC of orphaned `.norte-partial` staging (ADR 0012, #11) in
    /// directory `dir`: deletes partials whose age exceeds `older_than`.
    /// Recognizes them by their exact SHAPE, not by a bare prefix — a real
    /// `.norte-partial.backup` file is NEVER touched. Returns how many it
    /// deleted.
    ///
    /// Default no-op (`Ok(0)`): only providers with LOCAL staging
    /// implement it. It is NOT a user mutation → doesn't go through the
    /// journal.
    ///
    /// # Errors
    /// [`Error`] if `dir` can't be listed; individual deletion failures
    /// are counted as not-deleted, without aborting the sweep.
    async fn gc_partials(
        &self,
        dir: &VPath,
        older_than: std::time::Duration,
    ) -> Result<usize, Error> {
        let _ = (dir, older_than);
        Ok(0)
    }

    /// Restores from the OS's NATIVE trash the item whose original path is
    /// `original` (M3-2's undo of a `Trashed` with no `reversal_ref`, ADR
    /// 0009). Default `Unsupported`. Only the local provider implements
    /// it: matches by original path the MOST RECENT item and restores it.
    /// Fails cleanly if the platform doesn't list the trash, there's no
    /// match, or the destination is occupied.
    ///
    /// **It's the GUESSING path, and that's why it's barely used
    /// anymore**: choosing "the most recent with this original path" is
    /// exactly what restored the wrong file when undoing an overwrite.
    /// Since the freedesktop trash names its destination, a `trashed` on
    /// `file://` on Linux carries a `reversal_ref` and undo uses
    /// [`Provider::restore_from`]. What's left here are the journal's old
    /// entries and platforms whose trash names nothing.
    ///
    /// # Errors
    /// [`Error::Unsupported`] (default and platforms without trash
    /// listing); [`Error::NotFound`] with no match; [`Error::Conflict`]
    /// occupied destination.
    async fn restore_trashed(&self, original: &VPath) -> Result<(), Error> {
        let _ = original;
        Err(Error::Unsupported)
    }

    /// Creates a symlink at `link` pointing at `target` (raw bytes, as is
    /// — the provider doesn't interpret them). `kind` distinguishes
    /// file/dir where the OS requires it (Windows); unix ignores it.
    ///
    /// If `link` already exists: [`Error::Conflict`]. Providers without
    /// symlinks: [`Error::Unsupported`] (default) and WITHOUT the
    /// `SYMLINKS` capability.
    async fn symlink(&self, link: &VPath, target: &[u8], kind: SymlinkKind) -> Result<(), Error> {
        let _ = (link, target, kind);
        Err(Error::Unsupported)
    }

    /// Opens a write sink for a NEW file. If the destination already
    /// exists: [`Error::Conflict`] — the overwrite policy belongs to the
    /// core, not the provider. The bytes aren't visible at the final path
    /// until [`ByteSink::commit`].
    async fn write(&self, p: &VPath) -> Result<Box<dyn ByteSink>, Error>;

    /// Opens a sink that RESUMES a previous write to `p` (ADR 0012):
    /// returns the sink and how many bytes are ALREADY durable in the
    /// staging (`0` = starts from scratch). The sink APPENDS after those
    /// bytes; the engine reads the source from that offset.
    ///
    /// Cross-invocation resumption requires a staging with a STABLE name
    /// per destination (a provider that supports it finds it again).
    /// Default: `(write(p), 0)` — no resume, starts from scratch (correct
    /// and safe; the engine recopies the whole thing).
    ///
    /// The final destination must still not exist: if it already does,
    /// the same [`Error::Conflict`] as [`Self::write`].
    async fn open_resumable(&self, p: &VPath) -> Result<(Box<dyn ByteSink>, u64), Error> {
        Ok((self.write(p).await?, 0))
    }

    /// SHA-256 of the FIRST `len` bytes of `p`'s resumable staging (#35,
    /// `VerifyPolicy::Hash`): the engine compares it against the hash of
    /// the SAME prefix of the SOURCE before resuming — if they don't
    /// match, the source changed under its feet and the partial is discarded.
    ///
    /// `Ok(None)` = no digest available → the engine degrades `Hash` to
    /// `Length` (discards only if the partial is longer than the source).
    /// Two causes: the provider exposes no digest (default), or there's
    /// NO staging for `p` right now. A provider with local staging
    /// (local/sftp) or multipart (S3, per-part `ETag`) returns `Some`.
    /// `len` never exceeds what `open_resumable` reported as durable; if
    /// it somehow did, whatever's available gets hashed and (if it's
    /// less) `None` is returned.
    async fn partial_digest(&self, p: &VPath, len: u64) -> Result<Option<[u8; 32]>, Error> {
        let _ = (p, len);
        Ok(None)
    }

    /// Creates ONE directory (the parent must exist; the core composes
    /// `mkdir -p`). If it already exists: [`Error::Conflict`].
    async fn mkdir(&self, p: &VPath) -> Result<(), Error>;

    /// Deletes ONE node: a file, a symlink or an EMPTY directory
    /// (post-order walking belongs to the core). Non-empty directory:
    /// [`Error::Conflict`].
    ///
    /// GUARANTEE (contractual): over a symlink it deletes THE LINK, never
    /// its target (lstat/unlink semantics). The copy engine relies on
    /// this so that moving a tree with expanded links doesn't destroy the
    /// targets (issue #19).
    async fn remove(&self, p: &VPath) -> Result<(), Error>;

    /// Renames within THIS provider (cross-provider = copy+delete in the
    /// core). Atomic if the `RENAME_ATOMIC` capability is declared.
    /// If the destination exists: [`Error::Conflict`].
    async fn rename(&self, from: &VPath, to: &VPath) -> Result<(), Error>;

    /// Server-side copy of ONE file if the backend offers it (S3
    /// `CopyObject`, reflink/clonefile…). `None` = "I don't know how to do
    /// this, do it by streaming"; only consulted if the `SERVER_COPY`
    /// capability is declared.
    ///
    /// If the destination already exists: [`Error::Conflict`] — SAME
    /// policy as [`Self::write`]. A backend whose native copy overwrites
    /// by default (S3 `CopyObject`) MUST check first; never a silent
    /// overwrite.
    ///
    /// **Cancellation contract (#51, rule 3):** the caller may DROP this
    /// future mid-way (the engine races it against its token). The
    /// implementer guarantees a drop never leaves an unmarked VISIBLE
    /// partial at the destination (S3 satisfies this: `CopyObject` is
    /// atomic and an incomplete multipart doesn't publish an object). An
    /// effect that completes server-side AFTER the drop is an accepted
    /// ambiguity (the engine documents it, family #32). WATCH OUT for
    /// implementations over `spawn_blocking` (a future local reflink):
    /// dropping the future does NOT stop the thread — the copy would
    /// ALWAYS run to completion and "after the drop" would go from a rare
    /// race to a deterministic case; that implementation needs its own
    /// cancellation point.
    async fn copy_native(&self, from: &VPath, to: &VPath) -> Option<Result<(), Error>> {
        let _ = (from, to);
        None
    }

    /// Sets `p`'s POSIX permissions (#314, 0.60.0).
    ///
    /// `mode` is `chmod(2)`'s twelve bits and NOTHING else: the bits
    /// above say what class of node it is, and that doesn't change. The
    /// caller has already validated them, but a provider that receives
    /// others must still reject them instead of trimming them — trimming
    /// changes the permission to one nobody asked for.
    ///
    /// Over a SYMLINK it acts on what the link points at, which is what
    /// `chmod(2)` does and what whoever asks for it from a listing
    /// expects; a provider without that distinction has nothing to decide.
    ///
    /// The default is `Unsupported`, and that's the right answer for
    /// almost everyone: inside a `.zip` there are no permissions to
    /// change and an object bucket has no mode. Whoever implements it
    /// also declares
    /// [`CapabilityFlags::POSIX_MODE`](norte_proto::CapabilityFlags),
    /// which is what a frontend checks to turn off the gesture instead of
    /// offering it and failing.
    ///
    /// # Errors
    ///
    /// [`Error::Unsupported`] if this location has no POSIX permissions;
    /// [`Error::NotFound`] if the path isn't there; the backend's error if
    /// it couldn't (permission denied, read-only system).
    async fn set_mode(&self, p: &VPath, mode: u32) -> Result<(), Error> {
        let _ = (p, mode);
        Err(Error::Unsupported)
    }
}

/// Type of symlink to create: Windows distinguishes file/directory at
/// creation (`CreateSymbolicLinkW`); unix ignores it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SymlinkKind {
    /// The target is (or will be) a file.
    File,
    /// The target is (or will be) a directory.
    Dir,
    /// The caller does NOT know (e.g. the copy engine preserving a link
    /// from another provider, issue #18): the provider determines it
    /// best-effort by resolving the target IN ITS OWN tree — a broken or
    /// undeterminable target degrades to `File` (documented). On an OS
    /// where the kind doesn't matter (unix) this is equivalent to `File`
    /// at no cost.
    Unknown,
}

/// Resolve symlinks when computing a node's identity?
/// (Parameter of [`Provider::node_id`].)
///
/// ```
/// use norte_vfs::FollowLinks;
/// // `No` = the link's own identity; `Yes` = its target's.
/// assert_ne!(FollowLinks::No, FollowLinks::Yes);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FollowLinks {
    /// The node's own identity (lstat semantics, like `stat`).
    No,
    /// The resolved target's identity; a broken symlink = `NotFound`.
    Yes,
}

/// A node's real identity WITHIN a provider: comparable and hashable,
/// never interpretable nor serializable to the wire (it's a backend
/// detail; comparing `NodeId`s from different providers means nothing).
///
/// ```
/// use norte_vfs::NodeId;
/// let a = NodeId { volume: 1, index: 42 };
/// let b = NodeId { volume: 1, index: 42 };
/// assert_eq!(a, b);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeId {
    /// Uniqueness domain of the index (unix device, Windows volume
    /// serial; 0 if the backend doesn't distinguish volumes).
    pub volume: u64,
    /// The node's index within the volume (`ino`; 128 bits cover `ReFS`'s
    /// `FileId`).
    pub index: u128,
}

/// Operations under a root that can't be escaped (#164, ADR 0054).
///
/// Obtained from [`Provider::open_root`]. `rel` is ALWAYS relative to
/// that root and is never composed with it: the backend does the
/// resolving, holding the root open, and that's why between two
/// operations nobody can replace a component with a symlink that sends
/// the next one somewhere else.
///
/// A `rel` that would escape answers [`Error::Conflict`] with
/// [`norte_proto::ConflictKind::EscapesRoot`] — never [`Error::NotFound`],
/// which a caller answers by creating the parent, i.e. doing exactly what
/// this trait exists to prevent.
///
/// An EMPTY `rel` is the root itself, and NONE of these operations
/// address it: with no last segment there's no name to act on, so they
/// answer [`Error::InvalidPath`]. To ask about the root there's
/// [`Self::root_id`].
///
/// # What this surface does NOT cover, and it's worth not reading too much into it
///
/// Since #218 it also covers the `stat` that DECIDES a collision and the
/// `remove` that carries it out **in `norte_core::ops`**, which was the
/// destructive half of an overwrite and the only one still going by path.
/// Left out, and this is written-down debt, not coverage:
///
/// - **Deleting the SOURCE of a copy-based `move`**, which is just as
///   destructive and goes by path: the source doesn't hang off the
///   destination's root, and confining it would mean opening another one.
/// - **`RenameAuto`**, which probes candidate names by path up to a
///   thousand times before writing. The write itself IS confined and is
///   create-new, so it isn't an escape; what it can do is CHOOSE the name
///   by looking at another tree.
/// - **`copy_native`**, which writes by path, bypassing the whole root.
///   Today it's unreachable — only `norte-vfs-object` declares
///   `SERVER_COPY` and that provider doesn't implement `open_root` —, but
///   the day a provider has both, all of this gets disarmed without
///   anything creaking.
/// - **`DeleteTree` and `rename`.** They dodge the hole for their own
///   reasons — `DeleteTree` doesn't descend into symlinks, revalidation
///   is an `lstat` —, which is different from being confined.
/// - **A COPIED symlink pointing outside** stays inside the tree and is a
///   trap for any caller that later writes there WITHOUT a confined root.
///   It can't be followed through here — resolution rejects it —, but it
///   can by path.
#[async_trait]
pub trait ConfinedRoot: Send + Sync {
    /// Creates a directory at `rel`. Same contract as [`Provider::mkdir`].
    async fn mkdir(&self, rel: &[Segment]) -> Result<(), Error>;

    /// Opens a sink for `rel`. Same contract as [`Provider::write`],
    /// publication included: the step from staging to final is confined
    /// too, which is where the guarantee would leak otherwise.
    async fn write(&self, rel: &[Segment]) -> Result<Box<dyn ByteSink>, Error>;

    /// Can this root continue a partial of its own? (#297)
    ///
    /// The caller asks this BEFORE choosing a path, because the answer
    /// decides whether cancelling keeps the staging (`keep`) or throws it
    /// away (`abort`). Answering `true` without [`Self::open_resumable`]
    /// leaving a REDISCOVERABLE staging leaves a partial per attempt that
    /// nobody consumes.
    ///
    /// Default `false`: the `open_resumable` default below returns a
    /// normal `write`, whose staging has no reason to be rediscoverable.
    fn resumes(&self) -> bool {
        false
    }

    /// Same contract as [`Provider::open_resumable`]. Default: no resume,
    /// which is correct and safe (the engine recopies the whole thing).
    async fn open_resumable(&self, rel: &[Segment]) -> Result<(Box<dyn ByteSink>, u64), Error> {
        Ok((self.write(rel).await?, 0))
    }

    /// The identity of the NODE this root has open.
    ///
    /// Exists so the caller can check that the root it was given is the
    /// one it validated, and not another. Confinement's anchor is gotten
    /// by opening a PATH — `open_root` resolves it like any other,
    /// symlinks included, because a `~/backups -> /mnt/disk/backups` is
    /// legitimate and refusing it would break real trees —, so between
    /// validating that path and opening it there's the usual window.
    /// Everything AFTER that stays perfectly confined; what has to be
    /// ruled out is that it's confined to the wrong tree.
    ///
    /// `Ok(None)` = this backend has no stable identity, same as
    /// [`Provider::node_id`]. Then there's nothing to compare and the
    /// caller decides with what it has.
    ///
    /// # Errors
    /// Whatever looking at the already-open node produces.
    async fn root_id(&self) -> Result<Option<NodeId>, Error> {
        Ok(None)
    }

    /// The identity of the node at `rel`, VIA the root's DESCRIPTOR.
    ///
    /// Same contract as [`Provider::node_id`] with [`FollowLinks::No`] —
    /// describes the LINK, never its target, same as [`Self::stat`] —,
    /// and exists for the same reason the rest of this interface does:
    /// the logical path may have stopped leading here.
    ///
    /// That case isn't theoretical, it's #369. A copy writes via the
    /// destination root's descriptor; if that folder gets renamed to the
    /// trash while the copy is in progress, the descriptor stays valid
    /// and the bytes keep landing where they should, but
    /// `node_id(/destination/f0001)` answers [`Error::NotFound`] — the
    /// path no longer leads there. Asking by path leaves without
    /// identity exactly the entries that need it most.
    ///
    /// `Ok(None)` = this backend has no stable identity, same as
    /// [`Provider::node_id`] and [`Self::root_id`]. It's the default, and
    /// whoever gets it decides with what they have.
    ///
    /// # Errors
    /// [`Error::NotFound`] if there's nothing at `rel`; whatever looking
    /// at the node produces otherwise.
    async fn node_id(&self, rel: &[Segment]) -> Result<Option<NodeId>, Error> {
        let _ = rel;
        Ok(None)
    }

    /// Creates a symlink at `rel` pointing at `target`. Same contract as
    /// [`Provider::symlink`], `kind` included.
    ///
    /// It's here because copying a symlink is CREATING one at the
    /// destination, and that creation composes a path just like the
    /// other two: without this method, a copy whose source is a symlink
    /// would be left unconfined and the hole would stay open through the
    /// most common way there is to reach it.
    ///
    /// What's confined is WHERE the link lands, never where it points: a
    /// target that leaves the root is a broken symlink or one pointing
    /// outside, which is exactly what the source said and what
    /// `Preserve` promises to copy.
    ///
    /// # Errors
    /// [`Error::Conflict`] if `rel` is occupied or would escape the root;
    /// [`Error::Unsupported`] if this backend doesn't know how to create symlinks.
    async fn symlink(&self, rel: &[Segment], target: &[u8], kind: SymlinkKind)
    -> Result<(), Error>;

    /// Same contract as [`Provider::stat`]: describes the LINK, never its
    /// target.
    async fn stat(&self, rel: &[Segment]) -> Result<Entry, Error>;

    /// Deletes the LEAF at `rel`. Same contract as [`Provider::remove`]
    /// for a non-directory (#218).
    ///
    /// Exists because a copy with `Overwrite` or `Newer` **destroys
    /// before writing**, and that half was left out of confinement: the
    /// `write` went via the descriptor and the `remove` preceding it went
    /// by path. With `dest/sub` replaced by a bridge to another tree, the
    /// deletion used to take a file from OUTSIDE the approved root and
    /// only then would the confined write refuse — a destroyed file,
    /// nothing written in its place, and a journal entry naming a place
    /// that wasn't it.
    ///
    /// A directory is NOT deleted through here: replacing a dir with a
    /// leaf is `TypeMismatch`, which is an answer and not a policy.
    ///
    /// The default is [`Error::Unsupported`], and the caller has to treat
    /// it as such: a root that doesn't know how to delete makes
    /// `Overwrite` get REJECTED, never fall back to by-path deletion —
    /// that would reopen the hole exactly in the case this exists to close.
    ///
    /// # Errors
    /// [`Error::Unsupported`] if this backend doesn't know how to do a
    /// confined delete; [`Error::NotFound`] if there's nothing at `rel`;
    /// whatever the deletion produces otherwise.
    async fn remove(&self, rel: &[Segment]) -> Result<(), Error> {
        let _ = rel;
        Err(Error::Unsupported)
    }

    /// Deletes the EMPTY DIRECTORY at `rel` (#296).
    ///
    /// Twin of [`Self::mkdir`], and separate from [`Self::remove`] for
    /// the same reason `unlinkat` has `AT_REMOVEDIR`: they're two
    /// distinct effects and confusing them is how a tree gets deleted
    /// while believing a file was being deleted. A `Mirror`'s post-order
    /// deletion asks for it, reaching each directory once it's already
    /// empty.
    ///
    /// Same default and same contract for the caller as `remove`: a root
    /// that doesn't know how REJECTS the operation, never falls back to
    /// by-path deletion.
    ///
    /// # Errors
    /// [`Error::Unsupported`] if this backend doesn't know how; whatever
    /// the deletion produces otherwise — including whatever corresponds
    /// to a directory that isn't empty.
    async fn rmdir(&self, rel: &[Segment]) -> Result<(), Error> {
        let _ = rel;
        Err(Error::Unsupported)
    }

    /// The digest of the first `len` bytes of `rel`'s partial, VIA THE
    /// DESCRIPTOR (#297 revision).
    ///
    /// Twin of [`Provider::partial_digest`], and exists for the same
    /// reason as [`Self::stat`]: with a confined destination, that one
    /// resolves the staging's name BY PATH, so the only verification
    /// there is on the bytes being resumed (`VerifyPolicy::Hash`) used to
    /// go through the door confinement closed for `stat` and `remove`. A
    /// replaced intermediate component gives it the digest of ANOTHER
    /// file, and `Hash` stops verifying anything.
    ///
    /// `Ok(None)` = this backend doesn't know (same contract as
    /// `Provider`'s), and then the caller degrades to `Length`.
    ///
    /// # Errors
    /// Whatever reading the already-open partial produces.
    async fn partial_digest(&self, rel: &[Segment], len: u64) -> Result<Option<[u8; 32]>, Error> {
        let _ = (rel, len);
        Ok(None)
    }
}
