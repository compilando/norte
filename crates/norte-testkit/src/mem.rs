//! [`MemProvider`]: an in-memory simulated FS, deterministic (`BTreeMap` +
//! logical clock), with configurable capabilities and fault injection. It is
//! the test bench for the copy engine and the contractual suite (spec §12).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};

use async_trait::async_trait;
use bytes::Bytes;
use futures::StreamExt;
use norte_proto::{
    Authority, Capabilities, CapabilityFlags, ConflictKind, Entry, EntryKind, Error, Scheme, VPath,
};
use norte_vfs::{ByteSink, ByteStream, EntryStream, Provider};

use crate::faults::{Faults, SegPath, seg_path};

/// Chunk size for read streams (small on purpose: forces consumers to
/// handle multi-chunk even with test-sized contents).
const READ_CHUNK: usize = 1024;

/// The permissions this provider claims to have before anyone changes them
/// (#314): `0o644`, what a freshly-created file gets with `umask` 022.
const DEFAULT_MODE: u32 = 0o644;

#[derive(Debug, Clone)]
enum Node {
    File {
        content: Vec<u8>,
        mtime: i64,
        id: u64,
    },
    Dir {
        mtime: i64,
        id: u64,
    },
    Symlink {
        target: Vec<u8>,
        mtime: i64,
        id: u64,
        kind: norte_vfs::SymlinkKind,
    },
}

impl Node {
    /// The node's identity (issue #16): assigned on creation, stable across
    /// rename (the node moves key, it is not recreated).
    fn id(&self) -> u64 {
        match self {
            Node::File { id, .. } | Node::Dir { id, .. } | Node::Symlink { id, .. } => *id,
        }
    }
}

#[derive(Debug)]
struct Tree {
    /// Nodes by segment path; the root is implicit (always Dir).
    nodes: BTreeMap<SegPath, Node>,
    /// Per-destination resume staging (ADR 0012): bytes KEPT by a `keep`
    /// that a later `open_resumable` resumes. Cleared on commit/abort.
    partials: BTreeMap<SegPath, Vec<u8>>,
    /// POSIX permissions by path (#314). Absent = the default below, which
    /// is what a freshly-created file would have with `umask` 022.
    modes: BTreeMap<SegPath, u32>,
    /// Logical clock: advances by 1 per mutation → deterministic mtimes.
    clock: i64,
    /// Next node identity (0 is the implicit root).
    next_id: u64,
}

impl Default for Tree {
    fn default() -> Self {
        Self {
            nodes: BTreeMap::new(),
            partials: BTreeMap::new(),
            modes: BTreeMap::new(),
            clock: 0,
            next_id: 1,
        }
    }
}

impl Tree {
    fn tick(&mut self) -> i64 {
        self.clock += 1;
        self.clock
    }

    fn new_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }
}

/// In-memory provider for tests. Deterministic: the same operation script →
/// the same tree, the same mtimes (logical clock), the same listing order
/// (`BTreeMap`'s byte order).
///
/// # Simulator limits (read before writing collision tests)
///
/// - **Case folding is pure ASCII** (`eq_ignore_ascii_case`). `Ñ` and `ñ` do
///   NOT collide here, but they DO on NTFS (`$UpCase`) and APFS (Unicode
///   fold). A trailing byte of a legacy multibyte encoding (e.g. Shift-JIS
///   `83 65`) can produce a spurious `CaseCollision` against `83 45`. Do NOT
///   write tests that depend on Unicode case collision/non-collision against
///   this simulator.
/// - **Does not simulate normalization-insensitivity** (APFS: é NFC and NFD
///   are the same file). That axis will arrive as its own knob (M0 debt
///   issue).
/// - The input `VPath`'s scheme/authority is not validated: all authorities
///   share the same tree.
/// - **Traversal of intermediate symlinks only covers READS**
///   (stat/list/read/`read_link`/`node_id`): mutations
///   (write/mkdir/remove/rename/symlink) require literal Dir ancestors —
///   on real POSIX, mutating through a dir-symlink works. The copy engine
///   never mutates via paths through links (creations go to the real
///   destination tree; from an expanded link, THE LINK is what gets
///   deleted), so the testkit does not need it yet.
///
/// ```
/// use norte_testkit::MemProvider;
/// use norte_vfs::Provider;
///
/// let mem = MemProvider::new();
/// assert_eq!(mem.scheme(), "mem");
/// ```
pub struct MemProvider {
    caps: Capabilities,
    norm: Normalization,
    /// `false` = simulates a backend with no stable identity (`node_id` =
    /// None).
    node_ids: bool,
    /// Fixed value `list_skipped` returns (#93): simulates an archive
    /// provider that omitted entries from its index. `None` (default) =
    /// a backend that lists everything that exists.
    list_skipped: Option<u64>,
    /// A rename WITHOUT clobbering sees the destination's fold, like real
    /// filesystems do (#274).
    ///
    /// By default this provider ALLOWS `a → A` when the destination
    /// resolves to the source itself: it models an APFS `rename(2)`, which
    /// replaces and therefore changes the spelling without complaint. But
    /// norte does not rename with `rename(2)`: it renames without
    /// clobbering —`renameat2(RENAME_NOREPLACE)` on Linux,
    /// `renamex_np(RENAME_EXCL)` on macOS, `MoveFileExW` without replace on
    /// Windows— and there, a destination that resolves to the same node
    /// **exists**, so the rename fails with `EEXIST`.
    ///
    /// That difference is what left #274 untestable: the path the issue
    /// names did not reproduce because the double was more permissive than
    /// any real disk. With this knob, `Foo.txt → foo.txt` answers what an
    /// ext4 `+F` or an APFS would answer.
    noreplace_sees_fold: bool,
    /// LOGICAL trash (#99, closes debt H2): with it, `trash` moves the
    /// victim to `.norte-trash/<id>/payload` and returns `Some(payload)`
    /// (recoverable destination) instead of "vanish" trash (`None`). Models
    /// a remote provider with `logical_trash` (sftp/object) to test the
    /// idempotency and recovery of `reversal_ref`.
    logical_trash: bool,
    /// Synthetic attr catalogue (#108 block 2): empty (default) = a
    /// provider with no attrs; [`Self::with_synthetic_attrs`] populates it
    /// with deterministic, deliberately hostile values.
    attr_defs: Vec<norte_proto::AttrInfo>,
    /// Capabilities scripted PER LOCATION (ADR 0054): simulates a backend
    /// that serves more than one filesystem behind a scheme — the root on
    /// ext4 and a `/usb` on exFAT, or an ext4 directory under `+F`. Empty
    /// (default) = every location answers [`Self::capabilities`].
    caps_at: Arc<Mutex<BTreeMap<SegPath, Capabilities>>>,
    /// Paths someone asked about with `capabilities_at`, in order. Test
    /// seam: it is the only way to check that a path asks about the
    /// LOCATION and not the provider, when the two answers coincide.
    caps_at_asked: Arc<Mutex<Vec<SegPath>>>,
    tree: Arc<Mutex<Tree>>,
    faults: Arc<Faults>,
}

/// The simulated FS's Unicode normalization axis (issue #7).
///
/// Documented limit: with BOTH case AND normalization insensitive, a name
/// that differs in BOTH does not fold (the axes are evaluated separately;
/// real APFS combines them). Enough for the testkit.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Normalization {
    /// Bytes as-is (ext4): NFC and NFD are DIFFERENT names.
    #[default]
    ByteExact,
    /// Byte-preserving insensitive (APFS): NFC and NFD resolve to the same
    /// node; a normalization-only collision is labeled
    /// [`ConflictKind::Normalization`].
    Insensitive,
}

/// Name resolution config: both axes together.
#[derive(Debug, Clone, Copy)]
struct Lookup {
    case_sensitive: bool,
    norm_insensitive: bool,
}

impl MemProvider {
    /// Provider with the default capabilities of a "unix-like" FS:
    /// `RENAME_ATOMIC | CASE_SENSITIVE | CASE_PRESERVING`.
    #[must_use]
    pub fn new() -> Self {
        Self::with_flags(
            CapabilityFlags::RENAME_ATOMIC
                | CapabilityFlags::CASE_SENSITIVE
                | CapabilityFlags::CASE_PRESERVING
                | CapabilityFlags::SYMLINKS
                | CapabilityFlags::TRASH
                // #314: has POSIX permissions and they can be changed. This
                // is what lets `fs.set_mode`'s Task and its undo be tested
                // without touching disk.
                | CapabilityFlags::POSIX_MODE,
        )
    }

    /// Provider with custom flags (e.g. without `CASE_SENSITIVE` to simulate
    /// NTFS/APFS — with the limit that folding is ASCII, see the type's doc
    /// — or with `SERVER_COPY` to test `copy_native`).
    #[must_use]
    pub fn with_flags(flags: CapabilityFlags) -> Self {
        Self {
            caps: Capabilities {
                flags,
                max_path: None,
            },
            norm: Normalization::default(),
            node_ids: true,
            list_skipped: None,
            logical_trash: false,
            noreplace_sees_fold: false,
            // #314: `posix.mode` is ALWAYS present, because this provider
            // really emits it (it is not synthetic) and declares
            // `POSIX_MODE`. `with_synthetic_attrs` adds the `mem.*` ones for
            // whoever wants them.
            attr_defs: vec![norte_proto::AttrInfo {
                id: "posix.mode".to_owned(),
                label: "Mode".to_owned(),
                ty: norte_proto::AttrType::Uint,
                hint: norte_proto::AttrHint::Mode,
            }],
            caps_at: Arc::new(Mutex::new(BTreeMap::new())),
            caps_at_asked: Arc::new(Mutex::new(Vec::new())),
            tree: Arc::new(Mutex::new(Tree::default())),
            faults: Arc::new(Faults::default()),
        }
    }

    /// Scripts ONE location's capabilities (ADR 0054): from here on
    /// `capabilities_at(p)` answers `caps` instead of the backend's
    /// declaration. It is the seam that lets a `+F` or a mounted exFAT be
    /// tested without having either — no CI in this project has them.
    ///
    /// Only affects the EXACT location: one of its children still answers
    /// the declaration, because the testkit does not simulate mount
    /// inheritance and faking it would hide exactly the bug #153 describes.
    ///
    /// # Panics
    ///
    /// If the script's mutex was poisoned by an earlier panic — in a
    /// testkit that already means a broken test.
    pub fn set_caps_at(&self, p: &VPath, caps: Capabilities) {
        self.caps_at
            .lock()
            .expect("sound caps_at lock")
            .insert(seg_path(p), caps);
    }

    /// Did anyone ask about location `p` with `capabilities_at`?
    ///
    /// # Panics
    /// If the mutex was poisoned by an earlier panic — in a testkit that
    /// already means a broken test.
    #[must_use]
    pub fn was_asked_about(&self, p: &VPath) -> bool {
        self.caps_at_asked
            .lock()
            .expect("sound caps_at lock")
            .contains(&seg_path(p))
    }

    /// Forgets who asked, so a test can separate two phases (what the plan
    /// asked from what the undo asks).
    ///
    /// # Panics
    /// If the mutex was poisoned by an earlier panic.
    pub fn forget_who_asked(&self) {
        self.caps_at_asked
            .lock()
            .expect("sound caps_at lock")
            .clear();
    }

    /// Deterministic SYNTHETIC attributes (#108 block 2) with deliberately
    /// hostile values: a non-UTF-8 owner (`Bytes`), text with an RTL
    /// override + ZWJ, WIDE text (CJK + a ZWJ emoji family, #117
    /// encoding-audit L2). For testing plumbing and rendering without a
    /// real provider.
    #[must_use]
    pub fn with_synthetic_attrs(mut self) -> Self {
        use norte_proto::{AttrHint, AttrInfo, AttrType};
        let mk = |id: &str, label: &str, ty, hint| AttrInfo {
            id: id.to_owned(),
            label: label.to_owned(),
            ty,
            hint,
        };
        // ADDED to what is already there (`posix.mode`, #314), not
        // replacing it: this provider keeps emitting the mode with or
        // without synthetic attrs, and no longer announcing it would make
        // it emit what it does not declare.
        self.attr_defs.extend([
            mk("mem.owner", "Owner", AttrType::Bytes, AttrHint::Identity),
            mk("mem.note", "Note", AttrType::Text, AttrHint::Opaque),
            mk("mem.mode", "Mode", AttrType::Uint, AttrHint::Mode),
            mk("mem.stamp", "Stamp", AttrType::TimeMs, AttrHint::Timestamp),
            mk("mem.wide", "Wide", AttrType::Text, AttrHint::Opaque),
        ]);
        self
    }

    /// Turns on LOGICAL trash (#99): `trash` moves to
    /// `.norte-trash/<id>/payload` and returns `Some(payload)` instead of
    /// "vanish" trash. Requires the `TRASH` capability (which [`Self::new`]
    /// carries).
    #[must_use]
    pub fn with_logical_trash(mut self) -> Self {
        self.logical_trash = true;
        self
    }

    /// A rename without clobbering sees the destination's fold (#274):
    /// `Foo.txt → foo.txt` on a provider that is not case-sensitive answers
    /// `Conflict{Exists}`, which is what a real disk answers.
    ///
    /// See the `noreplace_sees_fold` field for why the default is the other
    /// way.
    #[must_use]
    pub fn with_folding_noreplace(mut self) -> Self {
        self.noreplace_sees_fold = true;
        self
    }

    /// Simulates a CONTAINER provider that omitted `n` entries from its
    /// index (#93): `list_skipped` returns `Ok(Some(n))` for any path. For
    /// testing the daemon/Backend/frontends plumbing without a real archive.
    #[must_use]
    pub fn with_list_skipped(mut self, n: u64) -> Self {
        self.list_skipped = Some(n);
        self
    }

    /// Simulates a backend WITHOUT a stable node identity (object storage,
    /// ftp): `node_id` always returns `Ok(None)`. For testing the engine's
    /// degraded paths (heuristics, Follow → Unsupported).
    #[must_use]
    pub fn without_node_ids(mut self) -> Self {
        self.node_ids = false;
        self
    }

    /// Test inspection (issue #18): the STORED kind of the symlink at `p`,
    /// already resolved if it was created with
    /// [`SymlinkKind`](norte_vfs::SymlinkKind) `::Unknown`. `None` if it
    /// does not exist or is not a symlink. Real providers do not expose
    /// this.
    #[must_use]
    pub fn symlink_kind_of(&self, p: &VPath) -> Option<norte_vfs::SymlinkKind> {
        let key = seg_path(p);
        let lk = self.lookup();
        let tree = self.lock();
        let real = resolve_traversing(&tree, lk, &key)?;
        match tree.nodes.get(&real) {
            Some(Node::Symlink { kind, .. }) => Some(*kind),
            _ => None,
        }
    }

    /// Sets the normalization axis (default: [`Normalization::ByteExact`]).
    ///
    /// ```
    /// use norte_testkit::{MemProvider, Normalization};
    /// let apfs = MemProvider::new().with_normalization(Normalization::Insensitive);
    /// let _ = apfs;
    /// ```
    #[must_use]
    pub fn with_normalization(mut self, norm: Normalization) -> Self {
        self.norm = norm;
        self
    }

    /// Fault-injection handle (shareable with the test while the provider
    /// is in use).
    #[must_use]
    pub fn faults(&self) -> Arc<Faults> {
        Arc::clone(&self.faults)
    }

    /// Shared body of `stat`/`stat_with` (#108 block 2): never delegate
    /// between them via the trait's defaults (recursion).
    async fn stat_inner(&self, p: &VPath, req: &norte_vfs::AttrRequest) -> Result<Entry, Error> {
        self.faults.op_gate().await?;
        let key = seg_path(p);
        let lk = self.lookup();
        let tree = self.lock();
        if key.is_empty() {
            return Ok(Entry {
                attrs: std::collections::BTreeMap::new(),
                path: p.clone(),
                kind: EntryKind::Dir,
                size: None,
                mtime_ms: Some(0),
            });
        }
        let real = resolve_traversing(&tree, lk, &key).ok_or(Error::NotFound)?;
        let node = tree.nodes.get(&real).ok_or(Error::NotFound)?;
        let mut entry = entry_for(p, &real, node, &self.attr_defs, req);
        // #314: the POSIX permissions, which this provider DOES have since
        // it declares `POSIX_MODE`. They stay outside `synthetic_attrs`
        // because they are not synthetic: it is real state that `set_mode`
        // writes, and a permission change's undo is tested by reading it.
        if req.wants("posix.mode") {
            let mode = tree.modes.get(&real).copied().unwrap_or(DEFAULT_MODE);
            entry.attrs.insert(
                "posix.mode".to_owned(),
                norte_proto::AttrValue::Uint(u64::from(mode)),
            );
        }
        Ok(entry)
    }

    /// Shared body of `list`/`list_with` (#108 block 2).
    async fn list_inner(
        &self,
        p: &VPath,
        req: &norte_vfs::AttrRequest,
    ) -> Result<EntryStream, Error> {
        self.faults.op_gate().await?;
        let key = seg_path(p);
        if self.faults.list_fails_for(&key) {
            return Err(Error::Io { retryable: true });
        }
        // Only a listing that IS served gets counted: a test that cancels
        // "after the n-th list" means n real listings, not n attempts.
        self.faults.tick_list();
        let lk = self.lookup();
        let tree = self.lock();
        // The child filter uses the REAL key: listing with different case
        // must see the same thing as stat (coherence with the simulated
        // FS). Like a real opendir, the traversal follows intermediate
        // symlinks AND the final link.
        let real = if key.is_empty() {
            key
        } else {
            let mut real = resolve_traversing(&tree, lk, &key).ok_or(Error::NotFound)?;
            if let Some(Node::Symlink { target, .. }) = tree.nodes.get(&real) {
                real = resolve_symlink(&tree, lk, &real, target).map_err(|_| Error::NotFound)?;
            }
            if !matches!(tree.nodes.get(&real), Some(Node::Dir { .. })) {
                return Err(Error::Conflict {
                    conflict: ConflictKind::TypeMismatch,
                });
            }
            real
        };
        // Children's paths hang off the REQUESTED path (with the leaf's
        // REAL name): listing through a link must give usable paths under
        // that link, as on a real FS.
        let entries: Vec<Result<Entry, Error>> = tree
            .nodes
            .iter()
            .filter(|(k, _)| k.len() == real.len() + 1 && k.starts_with(&real))
            .map(|(k, node)| {
                let name = k.last().expect("non-empty child key").clone();
                let seg = norte_proto::Segment::new(name).expect("tree key already validated");
                Ok(entry_for_child(p.join(seg), node, &self.attr_defs, req))
            })
            .collect();
        Ok(futures::stream::iter(entries).boxed())
    }

    /// This provider's root: `mem:///`.
    ///
    /// # Panics
    /// Never: the scheme is constant and valid.
    #[must_use]
    pub fn root() -> VPath {
        VPath::root(Scheme::new("mem").expect("valid constant scheme"), None)
    }

    fn lookup(&self) -> Lookup {
        Lookup {
            case_sensitive: self.caps.flags.contains(CapabilityFlags::CASE_SENSITIVE),
            norm_insensitive: self.norm == Normalization::Insensitive,
        }
    }

    fn lock(&self) -> MutexGuard<'_, Tree> {
        // Invariant: nobody panics with the lock held.
        self.tree.lock().expect("sound tree lock")
    }
}

impl Default for MemProvider {
    fn default() -> Self {
        Self::new()
    }
}

/// Equality with ASCII folding (the limits are documented on [`MemProvider`]).
fn fold_eq_path(a: &SegPath, b: &SegPath) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.eq_ignore_ascii_case(y))
}

/// `info_key`'s `.norte-info` decodes to exactly `p` (same victim): the
/// trash entry is OURS, not someone else's collision with the same id (#99,
/// rust review MAJOR). `info_decode` already requires the same
/// scheme+authority; here the full original path is compared.
fn info_matches(tree: &Tree, info_key: &SegPath, p: &VPath) -> bool {
    matches!(
        tree.nodes.get(info_key),
        Some(Node::File { content, .. })
            if norte_vfs::trash::info_decode(content, p)
                .is_ok_and(|i| i.original == *p)
    )
}

/// Same NFC shape, segment by segment? Only if both are valid UTF-8.
fn nfc_eq_path(a: &SegPath, b: &SegPath) -> bool {
    use unicode_normalization::UnicodeNormalization;
    a.len() == b.len()
        && a.iter().zip(b).all(
            |(x, y)| match (std::str::from_utf8(x), std::str::from_utf8(y)) {
                (Ok(x), Ok(y)) => x.nfc().eq(y.nfc()),
                _ => x == y,
            },
        )
}

/// Resolves `key` against the tree per the case and normalization axes.
/// Returns the REAL stored key (may differ in case or shape).
fn resolve(tree: &Tree, lk: Lookup, key: &SegPath) -> Option<SegPath> {
    if tree.nodes.contains_key(key) {
        return Some(key.clone());
    }
    if !lk.case_sensitive
        && let Some(k) = tree.nodes.keys().find(|k| fold_eq_path(k, key))
    {
        return Some(k.clone());
    }
    if lk.norm_insensitive {
        return tree.nodes.keys().find(|k| nfc_eq_path(k, key)).cloned();
    }
    None
}

/// Resolution of a full path with symlink TRAVERSAL on intermediate
/// components (POSIX semantics: a real FS resolves `link/child` through the
/// link). One level of link per component — link→link chains give `None`,
/// same documented limit as [`resolve_symlink`]. The LEAF is not followed
/// (lstat semantics, like `stat`).
fn resolve_traversing(tree: &Tree, lk: Lookup, key: &SegPath) -> Option<SegPath> {
    // Shortcut: the exact key exists (the overwhelmingly common case).
    if tree.nodes.contains_key(key) {
        return Some(key.clone());
    }
    let mut canon: SegPath = Vec::new();
    for (i, seg) in key.iter().enumerate() {
        let mut probe = canon.clone();
        probe.push(seg.clone());
        let real = resolve(tree, lk, &probe)?;
        if i < key.len() - 1
            && let Some(Node::Symlink { target, .. }) = tree.nodes.get(&real)
        {
            let resolved = resolve_symlink(tree, lk, &real, target).ok()?;
            if !matches!(tree.nodes.get(&resolved), Some(Node::Dir { .. })) {
                return None;
            }
            canon = resolved;
        } else {
            canon = real;
        }
    }
    Some(canon)
}

/// Canonicalizes `key` with respect to the STORED case: every ancestor
/// adopts its Dir's real case (and must exist as a Dir); the leaf keeps the
/// requested case (case-preserving). Without this, an insert via different
/// case would create orphans invisible to `list` — impossible on a real FS.
///
/// `None` if some ancestor is missing or is not a Dir.
fn canonical_key(tree: &Tree, lk: Lookup, key: &SegPath) -> Option<SegPath> {
    let mut canon: SegPath = Vec::with_capacity(key.len());
    for (i, seg) in key.iter().enumerate() {
        if i == key.len() - 1 {
            canon.push(seg.clone());
        } else {
            let mut probe = canon.clone();
            probe.push(seg.clone());
            let real = resolve(tree, lk, &probe)?;
            if !matches!(tree.nodes.get(&real), Some(Node::Dir { .. })) {
                return None;
            }
            canon = real;
        }
    }
    Some(canon)
}

/// Minimal resolution of a Mem symlink: `target` relative to the link's
/// PARENT, segments separated by `/`. No `..`, no absolutes, and
/// symlink→symlink CHAINS give `NotFound` (a real FS would follow them):
/// that is enough for the testkit — exotic targets are tested on the real
/// provider.
fn resolve_symlink(
    tree: &Tree,
    lk: Lookup,
    link: &SegPath,
    target: &[u8],
) -> Result<SegPath, Error> {
    if target.starts_with(b"/") || target.split(|b| *b == b'/').any(|s| s == b"..") {
        return Err(Error::Unsupported);
    }
    let mut key: SegPath = link[..link.len() - 1].to_vec();
    for seg in target.split(|b| *b == b'/').filter(|s| !s.is_empty()) {
        key.push(seg.to_vec());
    }
    resolve(tree, lk, &key).ok_or(Error::NotFound)
}

/// Classifies a collision: byte-exact = `Exists`; same NFC shape =
/// `Normalization` (issue #8); otherwise, a case variant = `CaseCollision`.
fn collision_kind(real: &SegPath, requested: &SegPath) -> ConflictKind {
    if real == requested {
        ConflictKind::Exists
    } else if nfc_eq_path(real, requested) {
        ConflictKind::Normalization
    } else {
        ConflictKind::CaseCollision
    }
}

/// Per-node DETERMINISTIC synthetic values (#108 block 2): a pure function
/// of (catalogue, request, kind, mtime). The values are deliberately
/// hostile — masking is the frontends' problem, not the provider's.
fn synthetic_attrs(
    defs: &[norte_proto::AttrInfo],
    req: &norte_vfs::AttrRequest,
    node: &Node,
) -> std::collections::BTreeMap<String, norte_proto::AttrValue> {
    use norte_proto::AttrValue;
    let mut out = std::collections::BTreeMap::new();
    if defs.is_empty() || req.is_empty() {
        return out;
    }
    let wants = |id: &str| defs.iter().any(|d| d.id == id) && req.wants(id);
    if wants("mem.owner") {
        // NON-UTF-8 owner: raw bytes, never a String (rule 1).
        out.insert(
            "mem.owner".to_owned(),
            AttrValue::Bytes(b"due\xf1o-\xff\xfe".to_vec()),
        );
    }
    if wants("mem.note") {
        // RTL override + ZWJ: smoke test for the frontends' masking.
        out.insert(
            "mem.note".to_owned(),
            AttrValue::Text("\u{202e}atón\u{202c} a\u{200d}b".to_owned()),
        );
    }
    if wants("mem.wide") {
        // WIDE text (#117 encoding-audit L2): CJK double-width + the SAME
        // ZWJ emoji family from the corpus (`emoji_zwj_family`, single
        // source) — a multi-codepoint grapheme to pin that a wide cell
        // never shifts the neighboring column in the frontends.
        let family = crate::corpus::hostile_names()
            .into_iter()
            .find(|n| n.id == "emoji_zwj_family")
            // The embedded corpus always carries the fixture (pure UTF-8);
            // if it were ever renamed, the value stays CJK-only and the
            // frontends' width pins would give it away.
            .and_then(|n| String::from_utf8(n.bytes).ok())
            .unwrap_or_default();
        out.insert(
            "mem.wide".to_owned(),
            AttrValue::Text(format!("日本語{family}")),
        );
    }
    if wants("mem.mode") {
        let mode = match node {
            Node::Dir { .. } => 0o040_755,
            Node::File { .. } => 0o100_644,
            Node::Symlink { .. } => 0o120_777,
        };
        out.insert("mem.mode".to_owned(), AttrValue::Uint(mode));
    }
    if wants("mem.stamp") {
        let mtime = match node {
            Node::File { mtime, .. } | Node::Dir { mtime, .. } | Node::Symlink { mtime, .. } => {
                *mtime
            }
        };
        out.insert("mem.stamp".to_owned(), AttrValue::TimeMs(mtime));
    }
    out
}

/// A child's [`Entry`] with its path ALREADY built (listings: the parent is
/// the REQUESTED path, not the canonical key — see `list`).
fn entry_for_child(
    path: VPath,
    node: &Node,
    defs: &[norte_proto::AttrInfo],
    req: &norte_vfs::AttrRequest,
) -> Entry {
    let attrs = synthetic_attrs(defs, req, node);
    match node {
        Node::File { content, mtime, .. } => Entry {
            attrs,
            path,
            kind: EntryKind::File,
            size: Some(content.len() as u64),
            mtime_ms: Some(*mtime),
        },
        Node::Dir { mtime, .. } => Entry {
            attrs,
            path,
            kind: EntryKind::Dir,
            size: None,
            mtime_ms: Some(*mtime),
        },
        Node::Symlink { mtime, .. } => Entry {
            attrs,
            path,
            kind: EntryKind::Symlink,
            size: None,
            mtime_ms: Some(*mtime),
        },
    }
}

/// Rebuilds a tree key's [`Entry`] over `base`'s scheme and authority (the
/// authority is preserved: a path's wire identity cannot change just by
/// going through the provider).
fn entry_for(
    base: &VPath,
    key: &SegPath,
    node: &Node,
    defs: &[norte_proto::AttrInfo],
    req: &norte_vfs::AttrRequest,
) -> Entry {
    let authority = base
        .authority()
        .map(|a| Authority::new(a).expect("authority already validated by VPath"));
    let mut p = VPath::root(
        Scheme::new(base.scheme()).expect("scheme already validated"),
        authority,
    );
    for seg in key {
        p = p.join(norte_proto::Segment::new(seg.clone()).expect("tree key already validated"));
    }
    entry_for_child(p, node, defs, req)
}

#[async_trait]
impl Provider for MemProvider {
    // The trait's signature is `-> &str`; returning a literal here is correct.
    #[expect(
        clippy::unnecessary_literal_bound,
        reason = "the trait's signature is `-> &str`"
    )]
    fn scheme(&self) -> &str {
        "mem"
    }

    fn capabilities(&self) -> Capabilities {
        self.caps
    }

    async fn capabilities_at(&self, p: &VPath) -> Result<Capabilities, Error> {
        let key = seg_path(p);
        self.caps_at_asked
            .lock()
            .expect("sound caps_at lock")
            .push(key.clone());
        Ok(self
            .caps_at
            .lock()
            .expect("sound caps_at lock")
            .get(&key)
            .copied()
            .unwrap_or(self.caps))
    }

    async fn stat(&self, p: &VPath) -> Result<Entry, Error> {
        self.stat_inner(p, &norte_vfs::AttrRequest::default()).await
    }

    async fn stat_with(&self, p: &VPath, opt: &norte_vfs::ListOptions) -> Result<Entry, Error> {
        self.stat_inner(p, &opt.attrs).await
    }

    async fn node_id(
        &self,
        p: &VPath,
        follow: norte_vfs::FollowLinks,
    ) -> Result<Option<norte_vfs::NodeId>, Error> {
        if !self.node_ids {
            return Ok(None);
        }
        self.faults.op_gate().await?;
        let key = seg_path(p);
        let lk = self.lookup();
        let tree = self.lock();
        // The implicit root has the reserved identity 0.
        if key.is_empty() {
            return Ok(Some(norte_vfs::NodeId {
                volume: 0,
                index: 0,
            }));
        }
        let real = resolve_traversing(&tree, lk, &key).ok_or(Error::NotFound)?;
        let node = tree.nodes.get(&real).ok_or(Error::NotFound)?;
        let node = match (follow, node) {
            (norte_vfs::FollowLinks::Yes, Node::Symlink { target, .. }) => {
                let resolved = resolve_symlink(&tree, lk, &real, target)?;
                match tree.nodes.get(&resolved) {
                    // link→link chain: consistent with read() — NotFound
                    // (Mem's minimal resolution does not follow chains).
                    Some(Node::Symlink { .. }) | None => return Err(Error::NotFound),
                    Some(n) => n,
                }
            }
            _ => node,
        };
        Ok(Some(norte_vfs::NodeId {
            volume: 0,
            index: u128::from(node.id()),
        }))
    }

    async fn list_skipped(&self, p: &VPath) -> Result<Option<u64>, Error> {
        let _ = p;
        Ok(self.list_skipped)
    }

    async fn list(&self, p: &VPath) -> Result<EntryStream, Error> {
        self.list_inner(p, &norte_vfs::AttrRequest::default()).await
    }

    async fn list_with(
        &self,
        p: &VPath,
        opt: &norte_vfs::ListOptions,
    ) -> Result<EntryStream, Error> {
        self.list_inner(p, &opt.attrs).await
    }

    /// The catalogue ALWAYS includes `posix.mode` (#314): this provider
    /// emits it and declares `POSIX_MODE`, and the shared contract requires
    /// that whatever gets emitted be announced — a double that broke the
    /// agreement it verifies would not be good for verifying anything.
    fn attrs(&self) -> &[norte_proto::AttrInfo] {
        &self.attr_defs
    }

    async fn read(
        &self,
        p: &VPath,
        range: Option<norte_proto::ByteRange>,
    ) -> Result<ByteStream, Error> {
        self.faults.op_gate().await?;
        self.faults.count_read();
        let key = seg_path(p);
        let lk = self.lookup();
        let tree = self.lock();
        let real = resolve_traversing(&tree, lk, &key).ok_or(Error::NotFound)?;
        let content = match tree.nodes.get(&real) {
            Some(Node::File { content, .. }) => content.clone(),
            // Like a real FS: read() FOLLOWS the symlink. Minimal
            // resolution (target relative to the link's parent, separated
            // by '/', no `..`): enough for testing the engine's Follow
            // policy.
            Some(Node::Symlink { target, .. }) => {
                let resolved = resolve_symlink(&tree, lk, &real, target)?;
                match tree.nodes.get(&resolved) {
                    Some(Node::File { content, .. }) => content.clone(),
                    Some(Node::Dir { .. }) => {
                        return Err(Error::Conflict {
                            conflict: ConflictKind::TypeMismatch,
                        });
                    }
                    _ => return Err(Error::NotFound),
                }
            }
            Some(Node::Dir { .. }) => {
                return Err(Error::Conflict {
                    conflict: ConflictKind::TypeMismatch,
                });
            }
            None => return Err(Error::NotFound),
        };
        drop(tree);

        // Range (ADR 0005): pread — an offset past EOF = empty, len is
        // clamped to EOF. The injected fault counts bytes FROM THE STREAM.
        let content: Vec<u8> = match range {
            None => content,
            Some(r) => {
                let start =
                    usize::try_from(r.offset.min(content.len() as u64)).unwrap_or(content.len());
                let end = match r.len {
                    None => content.len(),
                    Some(l) => start.saturating_add(usize::try_from(l).unwrap_or(usize::MAX)),
                }
                .min(content.len());
                content[start..end].to_vec()
            }
        };

        // Fault snapshot: the stream will truncate at the exact byte.
        let fail_at = self.faults.read_fault_for(&key);
        let mut chunks: Vec<Result<Bytes, Error>> = Vec::new();
        let mut emitted = 0usize;
        for chunk in content.chunks(READ_CHUNK) {
            if let Some(n) = fail_at
                && emitted + chunk.len() >= n
            {
                let take = n.saturating_sub(emitted);
                if take > 0 {
                    chunks.push(Ok(Bytes::copy_from_slice(&chunk[..take])));
                }
                chunks.push(Err(Error::Io { retryable: false }));
                return Ok(futures::stream::iter(chunks).boxed());
            }
            emitted += chunk.len();
            chunks.push(Ok(Bytes::copy_from_slice(chunk)));
        }
        Ok(futures::stream::iter(chunks).boxed())
    }

    async fn write(&self, p: &VPath) -> Result<Box<dyn ByteSink>, Error> {
        self.faults.op_gate().await?;
        let key = seg_path(p);
        if key.is_empty() {
            return Err(Error::InvalidPath);
        }
        let lk = self.lookup();
        let tree = self.lock();
        let canon = canonical_key(&tree, lk, &key).ok_or(Error::NotFound)?;
        if let Some(real) = resolve(&tree, lk, &canon) {
            return Err(Error::Conflict {
                conflict: collision_kind(&real, &canon),
            });
        }
        drop(tree);

        Ok(Box::new(MemSink {
            key: canon,
            buffer: Vec::new(),
            fail_at: self.faults.write_fault_for(&key),
            written: 0,
            tree: Arc::clone(&self.tree),
            lookup: lk,
            faults: Arc::clone(&self.faults),
        }))
    }

    async fn partial_digest(&self, p: &VPath, len: u64) -> Result<Option<[u8; 32]>, Error> {
        use sha2::{Digest, Sha256};
        self.faults.op_gate().await?;
        let key = seg_path(p);
        if key.is_empty() {
            return Err(Error::InvalidPath);
        }
        let lk = self.lookup();
        let tree = self.lock();
        let canon = canonical_key(&tree, lk, &key).ok_or(Error::NotFound)?;
        // No staging = no digest (the engine degrades to Length). With
        // staging, hashes EXACTLY the first `len` bytes (`len` never
        // exceeds what open_resumable reported, so the slice is valid).
        let Some(buffer) = tree.partials.get(&canon) else {
            return Ok(None);
        };
        let n = usize::try_from(len)
            .unwrap_or(buffer.len())
            .min(buffer.len());
        let digest = Sha256::digest(&buffer[..n]);
        Ok(Some(digest.into()))
    }

    async fn open_resumable(&self, p: &VPath) -> Result<(Box<dyn ByteSink>, u64), Error> {
        self.faults.op_gate().await?;
        let key = seg_path(p);
        if key.is_empty() {
            return Err(Error::InvalidPath);
        }
        let lk = self.lookup();
        let tree = self.lock();
        let canon = canonical_key(&tree, lk, &key).ok_or(Error::NotFound)?;
        // The final destination must still not exist (same contract as write).
        if let Some(real) = resolve(&tree, lk, &canon) {
            return Err(Error::Conflict {
                conflict: collision_kind(&real, &canon),
            });
        }
        // Resumes from the kept staging, if any.
        let buffer = tree.partials.get(&canon).cloned().unwrap_or_default();
        let already = buffer.len() as u64;
        drop(tree);
        Ok((
            Box::new(MemSink {
                key: canon,
                buffer,
                fail_at: self.faults.write_fault_for(&key),
                written: 0,
                tree: Arc::clone(&self.tree),
                lookup: lk,
                faults: Arc::clone(&self.faults),
            }),
            already,
        ))
    }

    /// #314: writes the mode into the side map. It is real state, not a
    /// simulation: a permission change's undo is checked by reading it back
    /// through `posix.mode`.
    async fn set_mode(&self, p: &VPath, mode: u32) -> Result<(), Error> {
        self.faults.op_gate().await?;
        let key = seg_path(p);
        let lk = self.lookup();
        let mut tree = self.lock();
        let real = resolve_traversing(&tree, lk, &key).ok_or(Error::NotFound)?;
        if !tree.nodes.contains_key(&real) {
            return Err(Error::NotFound);
        }
        tree.modes.insert(real, mode);
        Ok(())
    }

    async fn mkdir(&self, p: &VPath) -> Result<(), Error> {
        self.faults.op_gate().await?;
        let key = seg_path(p);
        if key.is_empty() {
            return Err(Error::Conflict {
                conflict: ConflictKind::Exists,
            });
        }
        let lk = self.lookup();
        let mut tree = self.lock();
        let canon = canonical_key(&tree, lk, &key).ok_or(Error::NotFound)?;
        if let Some(real) = resolve(&tree, lk, &canon) {
            return Err(Error::Conflict {
                conflict: collision_kind(&real, &canon),
            });
        }
        let mtime = tree.tick();
        let id = tree.new_id();
        tree.nodes.insert(canon, Node::Dir { mtime, id });
        drop(tree);
        self.ambiguous_gate()
    }

    async fn remove(&self, p: &VPath) -> Result<(), Error> {
        self.faults.op_gate().await?;
        let key = seg_path(p);
        if key.is_empty() {
            return Err(Error::Unsupported);
        }
        let lk = self.lookup();
        let mut tree = self.lock();
        let real = resolve(&tree, lk, &key).ok_or(Error::NotFound)?;
        if matches!(tree.nodes.get(&real), Some(Node::Dir { .. })) {
            let has_children = tree
                .nodes
                .keys()
                .any(|k| k.len() > real.len() && k.starts_with(&real));
            if has_children {
                return Err(Error::Conflict {
                    conflict: ConflictKind::TypeMismatch,
                });
            }
        }
        tree.nodes.remove(&real);
        // #314: and its mode. Leaving it would make a file later created
        // under that same name inherit the permissions of the one that is
        // gone, and an undo test would assert a mode this provider made up.
        tree.modes.remove(&real);
        tree.tick();
        drop(tree);
        self.ambiguous_gate()
    }

    async fn rename(&self, from: &VPath, to: &VPath) -> Result<(), Error> {
        self.faults.op_gate().await?;
        let from_key = seg_path(from);
        let to_key = seg_path(to);
        // BEFORE touching the tree: an injected fault does not apply its effect.
        if self.faults.rename_fails_from(&from_key) {
            return Err(Error::Io { retryable: false });
        }
        if from_key.is_empty() || to_key.is_empty() {
            return Err(Error::InvalidPath);
        }
        let lk = self.lookup();
        let mut tree = self.lock();
        let real_from = resolve(&tree, lk, &from_key).ok_or(Error::NotFound)?;
        let canon_to = canonical_key(&tree, lk, &to_key).ok_or(Error::NotFound)?;
        // Moving a dir INSIDE itself is impossible on any FS (EINVAL).
        if canon_to.len() > real_from.len() && canon_to.starts_with(&real_from) {
            return Err(Error::InvalidPath);
        }
        // The destination can "exist" only as the source itself with
        // different case (rename a→A on a case-insensitive-preserving FS):
        // allowed, unless this provider sees the fold when renaming WITHOUT
        // clobbering (#274), which is what a real disk does.
        if let Some(real_to) = resolve(&tree, lk, &canon_to)
            && (real_to != real_from || (self.noreplace_sees_fold && canon_to != real_from))
        {
            if !self.faults.renames_clobber() {
                return Err(Error::Conflict {
                    conflict: collision_kind(&real_to, &canon_to),
                });
            }
            // A provider that CLOBBERS (injected fault): the destination and
            // its subtree disappear, as a posix-rename would.
            let victims: Vec<SegPath> = tree
                .nodes
                .keys()
                .filter(|k| k.starts_with(&real_to))
                .cloned()
                .collect();
            for k in victims {
                tree.nodes.remove(&k);
            }
        }
        // Moves the node and its whole subtree.
        let moved: Vec<(SegPath, Node)> = tree
            .nodes
            .iter()
            .filter(|(k, _)| k.starts_with(&real_from))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        for (k, _) in &moved {
            tree.nodes.remove(k);
        }
        let mtime = tree.tick();
        for (k, mut node) in moved {
            let mut new_key = canon_to.clone();
            new_key.extend_from_slice(&k[real_from.len()..]);
            let (Node::Dir { mtime: m, .. }
            | Node::File { mtime: m, .. }
            | Node::Symlink { mtime: m, .. }) = &mut node;
            *m = mtime;
            // #314: the mode travels with the node, like the id — a rename
            // changes the name, not the permissions.
            if let Some(m) = tree.modes.remove(&k) {
                tree.modes.insert(new_key.clone(), m);
            }
            // The id travels INSIDE the node: rename preserves identity.
            tree.nodes.insert(new_key, node);
        }
        drop(tree);
        // The effect is already applied: if the test asked to cancel after
        // the n-th rename, this is IT (#274).
        self.faults.tick_rename();
        self.ambiguous_gate()
    }

    async fn trash(
        &self,
        p: &VPath,
        id: &norte_vfs::trash::TrashId,
    ) -> Result<Option<VPath>, Error> {
        if !self.caps.flags.contains(CapabilityFlags::TRASH) {
            return Err(Error::Unsupported);
        }
        self.faults.op_gate().await?;
        let key = seg_path(p);
        // The root is not trashed: the path is the problem (same as local).
        if key.is_empty() {
            return Err(Error::InvalidPath);
        }
        if self.logical_trash {
            return self.trash_logical(p, id);
        }
        let lk = self.lookup();
        let mut tree = self.lock();
        let real = resolve(&tree, lk, &key).ok_or(Error::NotFound)?;
        // Testkit's logical trash: the subtree disappears from view (real
        // list/restore = M3, on the real provider).
        let victims: Vec<SegPath> = tree
            .nodes
            .keys()
            .filter(|k| k.starts_with(&real))
            .cloned()
            .collect();
        for k in victims {
            tree.nodes.remove(&k);
        }
        tree.tick();
        drop(tree);
        // Test "vanish" trash: the subtree disappears from view, with no
        // recoverable destination exposed (like the OS's native trash). The
        // `ambiguous_gate` simulates the transient-after-effect (#17/#99):
        // the retry will see the victim absent and `trash_retrying`
        // degrades to `Ok(None)` (no `reversal_ref`, like native trash).
        self.ambiguous_gate()?;
        Ok(None)
    }

    /// Only the testkit's LOGICAL trash names its destination; "vanish"
    /// mimics macOS/Windows' native one and promises nothing.
    fn trash_restorable(&self) -> bool {
        self.logical_trash
    }

    async fn read_link(&self, p: &VPath) -> Result<Vec<u8>, Error> {
        self.faults.op_gate().await?;
        let key = seg_path(p);
        let lk = self.lookup();
        let tree = self.lock();
        let real = resolve_traversing(&tree, lk, &key).ok_or(Error::NotFound)?;
        match tree.nodes.get(&real) {
            Some(Node::Symlink { target, .. }) => Ok(target.clone()),
            Some(_) => Err(Error::Conflict {
                conflict: ConflictKind::TypeMismatch,
            }),
            None => Err(Error::NotFound),
        }
    }

    async fn symlink(
        &self,
        link: &VPath,
        target: &[u8],
        kind: norte_vfs::SymlinkKind,
    ) -> Result<(), Error> {
        self.faults.op_gate().await?;
        let key = seg_path(link);
        if key.is_empty() {
            return Err(Error::InvalidPath);
        }
        let lk = self.lookup();
        let mut tree = self.lock();
        let canon = canonical_key(&tree, lk, &key).ok_or(Error::NotFound)?;
        if let Some(real) = resolve(&tree, lk, &canon) {
            return Err(Error::Conflict {
                conflict: collision_kind(&real, &canon),
            });
        }
        // `Unknown` (issue #18): the provider resolves the kind against ITS
        // tree, best-effort — broken or unresolvable degrades to File.
        let kind = match kind {
            norte_vfs::SymlinkKind::Unknown => match resolve_symlink(&tree, lk, &canon, target) {
                Ok(resolved) => match tree.nodes.get(&resolved) {
                    Some(Node::Dir { .. }) => norte_vfs::SymlinkKind::Dir,
                    _ => norte_vfs::SymlinkKind::File,
                },
                Err(_) => norte_vfs::SymlinkKind::File,
            },
            explicit => explicit,
        };
        let mtime = tree.tick();
        let id = tree.new_id();
        tree.nodes.insert(
            canon,
            Node::Symlink {
                target: target.to_vec(),
                mtime,
                id,
                kind,
            },
        );
        drop(tree);
        self.ambiguous_gate()
    }

    async fn copy_native(&self, from: &VPath, to: &VPath) -> Option<Result<(), Error>> {
        if !self.caps.flags.contains(CapabilityFlags::SERVER_COPY) {
            return None;
        }
        Some(self.copy_native_inner(from, to).await)
    }
}

impl MemProvider {
    /// Exit gate for every point mutation: if a
    /// [`Faults::ambiguous_mutations`] charge is armed, the effect has
    /// ALREADY been applied and a transient error is returned anyway (issue
    /// #17).
    fn ambiguous_gate(&self) -> Result<(), Error> {
        if self.faults.take_ambiguous() {
            Err(Error::ProviderUnavailable { retryable: true })
        } else {
            Ok(())
        }
    }

    /// LOGICAL trash (#99): moves the victim to `.norte-trash/<id>/payload`
    /// and returns the recoverable destination. The deterministic `id`
    /// makes it IDEMPOTENT — if the victim is gone but the payload is there,
    /// this op already applied in an earlier transient attempt →
    /// `Some(payload)` (no victim and no payload = genuine `NotFound`). Dir
    /// markers and info are inserted directly (without consuming faults);
    /// the move reuses `rename`'s re-keying; a single
    /// [`Self::ambiguous_gate`] at the end simulates the
    /// transient-after-effect that `trash_retrying` recovers from.
    fn trash_logical(
        &self,
        p: &VPath,
        id: &norte_vfs::trash::TrashId,
    ) -> Result<Option<VPath>, Error> {
        let paths = norte_vfs::trash::plan(p, &id.as_segment())?;
        let trash_root = paths.dir.parent().ok_or(Error::Unsupported)?;
        let victim_key = seg_path(p);
        let dir_key = seg_path(&paths.dir);
        let root_key = seg_path(&trash_root);
        let info_key = seg_path(&paths.info);
        let payload_key = seg_path(&paths.payload);
        let lk = self.lookup();
        {
            let mut tree = self.lock();
            let Some(real_from) = resolve(&tree, lk, &victim_key) else {
                // Victim absent. Idempotency: payload present = it already
                // applied, but ONLY if the entry's `.norte-info` is OURS
                // (same victim). A FOREIGN entry with the same id is not
                // claimed (rust review MAJOR): a collision is reported, not
                // a payload that is not `p`'s. No payload = genuine
                // `NotFound`.
                if resolve(&tree, lk, &payload_key).is_none() {
                    return Err(Error::NotFound);
                }
                return if info_matches(&tree, &info_key, p) {
                    Ok(Some(paths.payload))
                } else {
                    Err(Error::Conflict {
                        conflict: ConflictKind::Exists,
                    })
                };
            };
            // The `<id>` entry already exists with FOREIGN info (a different
            // victim, same id): a real collision — its `.norte-info` is not
            // overwritten and the tree is not mixed. Absent or ours = keep
            // going (our partial).
            if tree.nodes.contains_key(&info_key) && !info_matches(&tree, &info_key, p) {
                return Err(Error::Conflict {
                    conflict: ConflictKind::Exists,
                });
            }
            let mtime = tree.tick();
            // `.norte-trash` and `.norte-trash/<id>` markers (idempotent).
            for dkey in [&root_key, &dir_key] {
                if !tree.nodes.contains_key(dkey) {
                    let id = tree.new_id();
                    tree.nodes.insert(dkey.clone(), Node::Dir { mtime, id });
                }
            }
            // `.norte-info` (overwriting a partial one is benign).
            let content = norte_vfs::trash::info_encode(p, id.deleted_ms());
            let info_id = tree.new_id();
            tree.nodes.insert(
                info_key,
                Node::File {
                    content,
                    mtime,
                    id: info_id,
                },
            );
            // Moves the victim subtree → payload, preserving identity.
            let moved: Vec<(SegPath, Node)> = tree
                .nodes
                .iter()
                .filter(|(k, _)| k.starts_with(&real_from))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            for (k, _) in &moved {
                tree.nodes.remove(k);
            }
            for (k, mut node) in moved {
                let mut new_key = payload_key.clone();
                new_key.extend_from_slice(&k[real_from.len()..]);
                let (Node::Dir { mtime: m, .. }
                | Node::File { mtime: m, .. }
                | Node::Symlink { mtime: m, .. }) = &mut node;
                *m = mtime;
                tree.nodes.insert(new_key, node);
            }
        }
        // The move has ALREADY been applied; the transient comes AFTER (#17).
        self.ambiguous_gate()?;
        Ok(Some(paths.payload))
    }

    async fn copy_native_inner(&self, from: &VPath, to: &VPath) -> Result<(), Error> {
        self.faults.op_gate().await?;
        // Specific gate (#51): simulates S3's long multipart copy — stays
        // pending until the test releases it or the caller cancels.
        self.faults.copy_native_gate().await;
        let from_key = seg_path(from);
        let to_key = seg_path(to);
        if to_key.is_empty() {
            return Err(Error::InvalidPath);
        }
        let lk = self.lookup();
        let mut tree = self.lock();
        let real_from = resolve(&tree, lk, &from_key).ok_or(Error::NotFound)?;
        let content = match tree.nodes.get(&real_from) {
            Some(Node::File { content, .. }) => content.clone(),
            // copy_native is for ONE file; trees (and symlinks, which have
            // their own policy) are composed by the core.
            Some(Node::Dir { .. } | Node::Symlink { .. }) => {
                return Err(Error::Conflict {
                    conflict: ConflictKind::TypeMismatch,
                });
            }
            None => return Err(Error::NotFound),
        };
        let canon_to = canonical_key(&tree, lk, &to_key).ok_or(Error::NotFound)?;
        if let Some(real) = resolve(&tree, lk, &canon_to) {
            return Err(Error::Conflict {
                conflict: collision_kind(&real, &canon_to),
            });
        }
        let mtime = tree.tick();
        let id = tree.new_id();
        tree.nodes
            .insert(canon_to, Node::File { content, mtime, id });
        Ok(())
    }
}

struct MemSink {
    key: SegPath,
    buffer: Vec<u8>,
    fail_at: Option<usize>,
    written: usize,
    tree: Arc<Mutex<Tree>>,
    lookup: Lookup,
    faults: Arc<Faults>,
}

#[async_trait]
impl ByteSink for MemSink {
    async fn write(&mut self, chunk: Bytes) -> Result<(), Error> {
        if let Some(n) = self.fail_at
            && self.written + chunk.len() >= n
        {
            let take = n.saturating_sub(self.written);
            self.buffer.extend_from_slice(&chunk[..take]);
            self.written += take;
            return Err(Error::Io { retryable: false });
        }
        self.written += chunk.len();
        self.buffer.extend_from_slice(&chunk);
        Ok(())
    }

    async fn commit(self: Box<Self>) -> Result<(), Error> {
        // Invariant: nobody panics with the lock held.
        let mut tree = self.tree.lock().expect("sound tree lock");
        // Full re-validation: between write() and commit() the parent could
        // have disappeared (→ NotFound, never orphans) or a collision could
        // have appeared.
        let canon = canonical_key(&tree, self.lookup, &self.key).ok_or(Error::NotFound)?;
        if let Some(real) = resolve(&tree, self.lookup, &canon) {
            return Err(Error::Conflict {
                conflict: collision_kind(&real, &canon),
            });
        }
        let mtime = tree.tick();
        let id = tree.new_id();
        tree.partials.remove(&canon);
        tree.nodes.insert(
            canon,
            Node::File {
                content: self.buffer.clone(),
                mtime,
                id,
            },
        );
        drop(tree);
        // The commit has ALREADY applied (staging→final rename): if an
        // ambiguous charge is armed, return transient AFTER the effect
        // (#32.1) — the "timeout after rename" of a remote provider.
        if self.faults.take_ambiguous() {
            return Err(Error::ProviderUnavailable { retryable: true });
        }
        Ok(())
    }

    async fn abort(self: Box<Self>) -> Result<(), Error> {
        // Also discards the kept staging (if there was any).
        self.tree
            .lock()
            .expect("sound tree lock")
            .partials
            .remove(&self.key);
        Ok(())
    }

    async fn keep(self: Box<Self>) -> Result<(), Error> {
        // Keeps the bytes for a later open_resumable (ADR 0012).
        self.tree
            .lock()
            .expect("sound tree lock")
            .partials
            .insert(self.key.clone(), self.buffer.clone());
        Ok(())
    }
}
