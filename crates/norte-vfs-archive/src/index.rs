//! In-RAM index of a compressed archive: a flat tree of entries with
//! structural name validation and anti-bomb limits (ADR 0018 C2/D2).

use std::collections::{BTreeSet, HashMap};

use norte_proto::{Entry, EntryKind, Error, Segment, VPath};

/// Index-building limits (ADR 0018 D2). Configurable since #95.2 via
/// [`ArchiveProvider::with_limits`](crate::ArchiveProvider::with_limits)
/// (the engine composes them from `Engine::set_archive_limits`; frontends
/// from `norte.toml`'s `[archive]` section).
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Ceiling on indexed entries (those omitted as hostile don't count).
    pub max_entries: usize,
    /// Byte ceiling on an entry's FULL name. Anti-bomb note (#60/D2): the
    /// `tar` crate MATERIALIZES a whole GNU longname in RAM BEFORE this
    /// check ever sees it — bounded by the container's own size (the
    /// longname is one entry's data), not by this ceiling.
    pub max_name_bytes: usize,
    /// Ceiling on an entry's path components.
    pub max_depth: usize,
    /// OBSOLETE since #59: the central directory is parsed in STREAMING
    /// fashion (never materialized nor retained — the zip locator is
    /// self-contained), so there's no CD memory left to govern. The field
    /// is kept for API compatibility and is NOT consulted. Historical: it
    /// used to be the ceiling on the cached CD (#61).
    #[deprecated(
        note = "obsolete since #59 (ADR 0030): the CD is parsed in streaming, nothing is retained — the field isn't consulted"
    )]
    pub max_cd_bytes: u64,
    /// TOTAL budget of DECOMPRESSED bytes for a `tar+gz`'s INDEX PASS (ADR
    /// 0028, #55): a gzip bomb is infinite CPU even if the pipeline's
    /// memory is streaming (the decoder never materializes the full
    /// content) — this ceiling cuts INDEXING short. No effect on
    /// `Format::Tar`/`Format::Zip`.
    ///
    /// An entry's `read` is NOT bounded by that entry's DECLARED size (fix
    /// from review #55: the earlier claim was FALSE): `size` can be as
    /// large as this very budget allows, and forward-decode ALWAYS starts
    /// from byte 0 of the stream — a `read`'s real cost is O(ABSOLUTE
    /// offset in the decompressed stream), documented next to
    /// `Locator::Gz`. The real bound is INDIRECT: if an entry's
    /// `offset`/`size` exceeded this budget, the INDEX PASS would already
    /// have failed trying to skip its body to locate the next entry
    /// (invariant: "over-budget entry ⇒ the WHOLE index fails") — a
    /// locator only ever reaches `read` if its position was ALREADY
    /// verified under this same ceiling during indexing.
    ///
    /// Exceeding it is reported as `Error::LimitExceeded`
    /// (`LIMIT_DECOMPRESSED_BYTES`) since #95.3 — an honest local limit,
    /// not a corruption verdict (`max_entries` likewise with
    /// `LIMIT_ENTRIES`; `max_name_bytes`/`max_depth` OMIT the entry as
    /// hostile, count in `skipped` and don't fail the index except via the
    /// omitted-entries budget).
    pub max_decompressed_bytes: u64,
    /// Budget of DECOMPRESSED bytes for a hot `tar+gz` container's SPOOL
    /// (#95.1): from the second read of the same container onward, the
    /// provider decompresses the WHOLE stream once into an ANONYMOUS
    /// temporary file and subsequent reads are local O(1) seeks instead of
    /// O(offset) forward-decode. A container whose decompressed size
    /// exceeds this ceiling is NOT spooled (it's remembered as
    /// non-spoolable until it changes generation) and its reads keep
    /// paying the usual forward-decode. `0` DISABLES the spool.
    ///
    /// NOT YET exposed in `norte.toml`'s `[archive]` section (the config
    /// channel arrives in a later phase); today it's only adjustable in
    /// code via
    /// [`ArchiveProvider::with_limits`](crate::ArchiveProvider::with_limits).
    pub spool_max_bytes: u64,
    /// Ceiling on nested archive LAYERS (#56, ADR 0018 A3): `1` = only a
    /// plain `zip+file`, `2` = zip inside tar, etc. The ENGINE applies it
    /// before composing (addressing is syntactically unbounded); exceeding
    /// it answers `Error::LimitExceeded` (`LIMIT_NESTING`). Every layer
    /// above a `tar+gz` pays forward-decode per read — the default is
    /// deliberately short.
    pub max_nesting: usize,
}

impl Default for Limits {
    #[allow(deprecated)] // initializes the obsolete field for API compat
    fn default() -> Self {
        Self {
            max_entries: 500_000,
            max_name_bytes: 4_096,
            max_depth: 64,
            max_cd_bytes: 8 * 1024 * 1024,
            max_decompressed_bytes: 64 * 1024 * 1024 * 1024,
            spool_max_bytes: 1024 * 1024 * 1024,
            max_nesting: 3,
        }
    }
}

/// Where an entry's bytes live inside the container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Locator {
    /// tar: the data is CONTIGUOUS and uncompressed — `read` is a range
    /// passthrough to the inner provider.
    Tar { offset: u64, size: u64 },
    /// zip (#59): a SELF-CONTAINED locator — everything `read` needs
    /// without retaining any archive object nor re-parsing the CD: the
    /// LOCAL header at `header_offset` resolves the real data offset and
    /// decompression (stored/deflate) runs in a blocking thread.
    Zip {
        /// The LOCAL header's offset in the container.
        header_offset: u64,
        /// Compression method (0 stored / 8 deflate).
        method: u16,
        /// CRC-32 the CD declares (verified on complete reads).
        crc32: u32,
        /// Compressed size.
        comp_size: u64,
        /// Decompressed size.
        uncomp_size: u64,
    },
    /// tar.gz/tgz (ADR 0028, #55): gz isn't seekable — `read` is
    /// FORWARD-DECODE from a fresh decoder that discards up to `offset`.
    /// `offset`/`size` are of the DECOMPRESSED stream, NOT of the
    /// compressed container's bytes (unlike `Tar`); they aren't validated
    /// against the container's size when indexing (that size is the
    /// COMPRESSED one and bounds nothing about the decompressed stream) —
    /// truncation is detected fail-loud in `read` itself (a premature
    /// EOF), never silently short data.
    Gz { offset: u64, size: u64 },
}

/// A zip triplet kept for attrs (#108 block 2), INDEPENDENT of the
/// `Locator` (which only exists for readable entries): method/crc/packed
/// are kept EVEN on encrypted or unsupported-method entries — precisely
/// where `archive.method` matters most. `None` on tar/tar.gz, dirs and symlinks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ZipExtra {
    pub method: u16,
    pub crc32: u32,
    pub comp_size: u64,
}

/// A node of the virtual tree.
#[derive(Debug, Clone)]
pub(crate) struct Node {
    pub kind: EntryKind,
    pub size: Option<u64>,
    pub mtime_ms: Option<i64>,
    /// `None` on dirs, symlinks and listable-but-unreadable entries
    /// (unsupported compression method, encrypted): `read` → `Unsupported`.
    pub locator: Option<Locator>,
    /// A tar symlink's raw target.
    pub link_target: Option<Vec<u8>>,
    /// Per-entry zip metadata for attrs (#108 block 2).
    pub zip: Option<ZipExtra>,
}

impl Node {
    pub(crate) fn dir(mtime_ms: Option<i64>) -> Self {
        Self {
            kind: EntryKind::Dir,
            size: None,
            mtime_ms,
            locator: None,
            link_target: None,
            zip: None,
        }
    }
}

/// Tree key: the inner path's components, in bytes.
pub(crate) type InnerPath = Vec<Vec<u8>>;

/// The complete index of ONE container, tied to a generation of the outer one.
pub(crate) struct ArchiveIndex {
    pub nodes: HashMap<InnerPath, Node>,
    /// inner dir → names of its direct children (ordered by bytes:
    /// deterministic and O(log n) per insertion — a plain tar with 500k
    /// entries can't cost O(n²), phase 8d finding B1).
    pub children: HashMap<InnerPath, BTreeSet<Vec<u8>>>,
    /// Entries omitted for a hostile name/per-entry limits (a signal; the
    /// detail goes through `tracing::warn!`).
    pub skipped: u64,
    /// The container's `(mtime_ms, size)` at index time — the invalidation key.
    pub generation: (Option<i64>, Option<u64>),
}

impl ArchiveIndex {
    pub(crate) fn new(generation: (Option<i64>, Option<u64>)) -> Self {
        Self {
            nodes: HashMap::new(),
            children: HashMap::new(),
            skipped: 0,
            generation,
        }
    }

    /// Validates and splits an entry's raw name. `None` = hostile (with
    /// the reason already logged via `warn!`).
    fn split_name(&mut self, raw: &[u8], limits: &Limits) -> Option<(InnerPath, bool)> {
        // debug! per entry (a hostile tar carries MILLIONS): the
        // aggregated warn! with the total is emitted by build_index at the end.
        let mut hostile = |why: &str| {
            tracing::debug!(name = ?String::from_utf8_lossy(raw), why, "entry omitted");
            self.skipped += 1;
            None
        };
        if raw.is_empty() {
            return hostile("empty name");
        }
        if raw.len() > limits.max_name_bytes {
            return hostile("name too long");
        }
        if raw.first() == Some(&b'/') {
            return hostile("absolute path");
        }
        let is_dir = raw.last() == Some(&b'/');
        let body = if is_dir { &raw[..raw.len() - 1] } else { raw };
        let mut parts: InnerPath = Vec::new();
        for comp in body.split(|&b| b == b'/') {
            if comp.is_empty() || comp == b"." || comp == b".." {
                return hostile("`.`/`..`/empty component (traversal)");
            }
            if comp == b"!" {
                return hostile("`!` component (ADR 0018 marker, unaddressable)");
            }
            if Segment::new(comp.to_vec()).is_err() {
                return hostile("component invalid as a VPath segment");
            }
            parts.push(comp.to_vec());
        }
        if parts.is_empty() {
            return hostile("name with no components");
        }
        if parts.len() > limits.max_depth {
            return hostile("excessive depth");
        }
        Some((parts, is_dir))
    }

    fn add_child(&mut self, parent: InnerPath, name: Vec<u8>) {
        self.children.entry(parent).or_default().insert(name);
    }

    /// Materializes `path`'s ancestors as implicit dirs. A pre-existing
    /// File at an ancestor position gets PROMOTED to a dir (same "dir
    /// wins" criterion as direct collisions: without this, `file a`
    /// followed by `a/child` would leave the subtree invisible — audit
    /// 8e, H4).
    fn ensure_parents(&mut self, path: &[Vec<u8>]) {
        for depth in 0..path.len().saturating_sub(1) {
            let dir: InnerPath = path[..=depth].to_vec();
            let node = self.nodes.entry(dir).or_insert_with(|| Node::dir(None));
            if node.kind != EntryKind::Dir {
                *node = Node::dir(node.mtime_ms);
                self.skipped += 1;
                tracing::debug!("file at an ancestor position gets promoted to a dir");
            }
            self.add_child(path[..depth].to_vec(), path[depth].clone());
        }
    }

    /// Inserts a container entry. Hostile names are omitted (skip+warn,
    /// ADR 0018 C2); exceeding `max_entries` cuts indexing short.
    ///
    /// Collision rules: last one wins (zip semantics); a dir wins over a
    /// file at the same path (a known attack pattern) and a dir is never
    /// demoted to a file.
    pub(crate) fn insert_entry(
        &mut self,
        raw_name: &[u8],
        node: Node,
        limits: &Limits,
    ) -> Result<(), Error> {
        let Some((path, trailing_slash)) = self.split_name(raw_name, limits) else {
            return Ok(());
        };
        // A trailing `/` overrides the declared kind (real zips do this).
        let node = if trailing_slash && node.kind != EntryKind::Dir {
            Node::dir(node.mtime_ms)
        } else {
            node
        };
        self.ensure_parents(&path);
        match self.nodes.get(&path) {
            Some(prev) if prev.kind == EntryKind::Dir && node.kind != EntryKind::Dir => {
                // A dir (explicit or implicit with children) is never
                // demoted to a file: we'd lose the subtree (attack pattern).
                tracing::debug!(
                    name = ?String::from_utf8_lossy(raw_name),
                    "file entry collides with a dir: the dir wins"
                );
                self.skipped += 1;
                return Ok(());
            }
            Some(_) => {
                tracing::debug!(
                    name = ?String::from_utf8_lossy(raw_name),
                    "duplicate entry: the last one wins"
                );
            }
            None => {}
        }
        self.nodes.insert(path.clone(), node);
        let (parent, name) = (
            path[..path.len() - 1].to_vec(),
            path.last().expect("non-empty path").clone(),
        );
        self.add_child(parent, name);
        // Budget checked AFTER inserting: implicit dirs count too (a
        // deep-paths bomb can't sneak in that way). The whole build is
        // discarded on the first excess.
        if self.nodes.len() > limits.max_entries {
            tracing::warn!(max = limits.max_entries, "index exceeds max_entries");
            // #95.3: a LOCAL limit, not corruption — the container may be
            // perfectly valid; norte refuses to pay for it.
            return Err(Error::LimitExceeded {
                limit: Error::LIMIT_ENTRIES.into(),
            });
        }
        Ok(())
    }

    /// A node's wire `Entry` (or the container's synthetic root).
    pub(crate) fn entry_for(
        &self,
        at: &VPath,
        inner: &[Vec<u8>],
        req: &norte_vfs::AttrRequest,
    ) -> Result<Entry, Error> {
        if inner.is_empty() {
            return Ok(Entry {
                attrs: std::collections::BTreeMap::new(),
                path: at.clone(),
                kind: EntryKind::Dir,
                size: None,
                mtime_ms: self.generation.0,
            });
        }
        let node = self.nodes.get(inner).ok_or(Error::NotFound)?;
        Ok(Entry {
            attrs: zip_attrs(node.zip.as_ref(), req),
            path: at.clone(),
            kind: node.kind,
            size: node.size,
            mtime_ms: node.mtime_ms,
        })
    }
}

/// Materializes the requested zip attrs (#108 block 2) from the triplet
/// kept in the index: zero I/O at query time.
fn zip_attrs(
    extra: Option<&ZipExtra>,
    req: &norte_vfs::AttrRequest,
) -> std::collections::BTreeMap<String, norte_proto::AttrValue> {
    use norte_proto::AttrValue;
    let mut out = std::collections::BTreeMap::new();
    let Some(z) = extra else {
        return out;
    };
    if req.wants("archive.method") {
        out.insert(
            "archive.method".to_owned(),
            AttrValue::Text(zip_method_name(z.method)),
        );
    }
    if req.wants("archive.packed_size") {
        out.insert(
            "archive.packed_size".to_owned(),
            AttrValue::Uint(z.comp_size),
        );
    }
    if req.wants("archive.crc32") {
        out.insert(
            "archive.crc32".to_owned(),
            AttrValue::Uint(u64::from(z.crc32)),
        );
    }
    out
}

/// Human name of the zip method (APPNOTE §4.4.5); unknown = "method-N",
/// never fails. It's an ASCII vocabulary id, not text from the archive.
fn zip_method_name(m: u16) -> String {
    match m {
        0 => "store".to_owned(),
        8 => "deflate".to_owned(),
        9 => "deflate64".to_owned(),
        12 => "bzip2".to_owned(),
        14 => "lzma".to_owned(),
        93 => "zstd".to_owned(),
        95 => "xz".to_owned(),
        99 => "aes".to_owned(),
        n => format!("method-{n}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file_node() -> Node {
        Node {
            kind: EntryKind::File,
            size: Some(1),
            mtime_ms: Some(0),
            locator: Some(Locator::Tar { offset: 0, size: 1 }),
            link_target: None,
            zip: None,
        }
    }

    fn idx() -> ArchiveIndex {
        ArchiveIndex::new((Some(0), Some(100)))
    }

    fn key(parts: &[&[u8]]) -> InnerPath {
        parts.iter().map(|p| p.to_vec()).collect()
    }

    #[test]
    fn inserts_and_materializes_parents() {
        let mut i = idx();
        i.insert_entry(b"a/b/c.txt", file_node(), &Limits::default())
            .expect("ok");
        assert_eq!(i.nodes[&key(&[b"a"])].kind, EntryKind::Dir);
        assert_eq!(i.nodes[&key(&[b"a", b"b"])].kind, EntryKind::Dir);
        assert_eq!(i.nodes[&key(&[b"a", b"b", b"c.txt"])].kind, EntryKind::File);
        assert!(i.children[&key(&[])].contains(b"a".as_slice()));
        assert!(i.children[&key(&[b"a", b"b"])].contains(b"c.txt".as_slice()));
        assert_eq!(i.skipped, 0);
    }

    #[test]
    fn omits_traversal_absolutes_and_the_marker() {
        let mut i = idx();
        let l = Limits::default();
        for hostile in [
            b"../evil".as_slice(),
            b"/etc/passwd",
            b"a/../b",
            b"a//b",
            b"a/./b",
            b"",
            b"!",
            b"a/!/b",
            b"nul\x00byte",
            b"/",
            b"a//",
        ] {
            i.insert_entry(hostile, file_node(), &l)
                .expect("skip, no err");
        }
        assert!(i.nodes.is_empty(), "nothing hostile enters the tree");
        assert_eq!(i.skipped, 11);
    }

    #[test]
    fn backslash_and_raw_bytes_are_preserved() {
        // `\` is NOT a separator (rule 1: bytes as is); raw cp437 gets in.
        let mut i = idx();
        let l = Limits::default();
        i.insert_entry(b"dir\\file", file_node(), &l).expect("ok");
        i.insert_entry(b"CAF\x82.TXT", file_node(), &l).expect("ok");
        assert!(i.nodes.contains_key(&key(&[b"dir\\file"])));
        assert!(i.nodes.contains_key(&key(&[b"CAF\x82.TXT"])));
        assert_eq!(i.skipped, 0);
    }

    #[test]
    fn duplicate_last_wins() {
        let mut i = idx();
        let l = Limits::default();
        i.insert_entry(b"x", file_node(), &l).expect("ok");
        let mut second = file_node();
        second.size = Some(99);
        i.insert_entry(b"x", second, &l).expect("ok");
        assert_eq!(i.nodes[&key(&[b"x"])].size, Some(99));
        assert_eq!(i.children[&key(&[])].len(), 1, "no duplicate children");
    }

    #[test]
    fn dir_wins_over_file() {
        let mut i = idx();
        let l = Limits::default();
        // file first, dir after: the dir replaces it.
        i.insert_entry(b"a", file_node(), &l).expect("ok");
        i.insert_entry(b"a/", file_node(), &l).expect("ok");
        assert_eq!(i.nodes[&key(&[b"a"])].kind, EntryKind::Dir);
        // dir first (implicit via a child), file after: the dir wins.
        i.insert_entry(b"b/child", file_node(), &l).expect("ok");
        i.insert_entry(b"b", file_node(), &l).expect("ok");
        assert_eq!(i.nodes[&key(&[b"b"])].kind, EntryKind::Dir);
        assert!(i.nodes.contains_key(&key(&[b"b", b"child"])));
        // FILE first, child after (H4): the file gets PROMOTED to a dir
        // and the subtree is visible.
        i.insert_entry(b"c", file_node(), &l).expect("ok");
        i.insert_entry(b"c/child", file_node(), &l).expect("ok");
        assert_eq!(i.nodes[&key(&[b"c"])].kind, EntryKind::Dir);
        assert!(i.nodes.contains_key(&key(&[b"c", b"child"])));
        assert!(i.children[&key(&[b"c"])].contains(b"child".as_slice()));
    }

    #[test]
    fn max_entries_cuts_short_with_io() {
        let mut i = idx();
        let l = Limits {
            max_entries: 2,
            ..Limits::default()
        };
        i.insert_entry(b"one", file_node(), &l).expect("ok");
        i.insert_entry(b"two", file_node(), &l).expect("ok");
        assert_eq!(
            i.insert_entry(b"three", file_node(), &l).unwrap_err(),
            Error::LimitExceeded {
                limit: Error::LIMIT_ENTRIES.into()
            }
        );
    }

    #[test]
    fn per_entry_limits_omit() {
        let mut i = idx();
        let l = Limits {
            max_name_bytes: 8,
            max_depth: 2,
            ..Limits::default()
        };
        i.insert_entry(b"very-long-name-here", file_node(), &l)
            .expect("skip");
        i.insert_entry(b"a/b/c", file_node(), &l).expect("skip");
        assert!(i.nodes.is_empty());
        assert_eq!(i.skipped, 2);
    }
}
