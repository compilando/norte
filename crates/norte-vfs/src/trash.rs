//! Logical `.norte-trash/` trash for providers without native trash
//! (ADR 0019). PURE helpers, no I/O: providers build the paths and
//! metadata with these functions and carry out the relocation with their
//! own primitives (rename on sftp, copy+delete on object).

use norte_proto::{Error, Segment, VPath};

/// The logical trash's root directory inside a connection.
pub const TRASH_DIR: &[u8] = b".norte-trash";
/// Restoration metadata file inside each entry.
pub const INFO_NAME: &[u8] = b".norte-info";
/// Version header of the `.norte-info` file.
const INFO_HEADER: &str = "norte-trash-info v1";

/// Unique, sortable identifier of a trash entry: `<deleted_ms>-<counter>`.
/// `counter` is monotonic per session to break ties between deletions in
/// the same millisecond. Always a valid [`Segment`] (only digits and `-`).
#[must_use]
pub fn trash_id(deleted_ms: u64, counter: u64) -> String {
    format!("{deleted_ms}-{counter}")
}

/// Identifier of a trashing operation, generated ONCE by the engine (#99)
/// and passed to [`crate::Provider::trash`]. Carries the `deleted_ms` (for
/// the `.norte-info`) and formats to the SAME segment as [`trash_id`], so
/// a retry after a transient failure points at the same deterministic
/// `.norte-trash/<id>/` destination — the basis for idempotence and for
/// undo recovering the `reversal_ref`.
///
/// ```
/// use norte_vfs::trash::TrashId;
/// let id = TrashId::new(1_726_000_000_123, 5);
/// assert_eq!(id.as_segment(), "1726000000123-5");
/// assert_eq!(id.deleted_ms(), 1_726_000_000_123);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrashId {
    deleted_ms: u64,
    counter: u64,
}

impl TrashId {
    /// A new id from the wall clock and a session-monotonic counter (the
    /// engine generates it once per operation).
    #[must_use]
    pub fn new(deleted_ms: u64, counter: u64) -> Self {
        Self {
            deleted_ms,
            counter,
        }
    }

    /// Deletion millisecond that goes into the `.norte-info` (M3's restore).
    #[must_use]
    pub fn deleted_ms(&self) -> u64 {
        self.deleted_ms
    }

    /// `<deleted_ms>-<counter>` segment for the entry's path; always a
    /// valid [`Segment`].
    #[must_use]
    pub fn as_segment(&self) -> String {
        trash_id(self.deleted_ms, self.counter)
    }
}

/// Absolute paths of a trash entry for a path about to be deleted.
#[derive(Debug, Clone)]
pub struct TrashPaths {
    /// The entry's directory: `.norte-trash/<id>/`.
    pub dir: VPath,
    /// The moved payload: `.norte-trash/<id>/<original-basename>`.
    pub payload: VPath,
    /// The metadata: `.norte-trash/<id>/.norte-info`.
    pub info: VPath,
}

/// Builds the trash paths for `p` under its connection's root.
///
/// `id` must come from [`trash_id`] (or any valid [`Segment`]).
///
/// # Errors
/// - [`Error::Unsupported`] if `p` is the provider's root (no basename):
///   the connection's root is never trashed.
/// - [`Error::InvalidPath`] if `id` isn't a valid segment.
pub fn plan(p: &VPath, id: &str) -> Result<TrashPaths, Error> {
    let basename = p.file_name().ok_or(Error::Unsupported)?.clone();
    // The trash itself, or anything inside it, is never trashed: avoids
    // renaming `.norte-trash` inside itself (POSIX EINVAL) and
    // self-referential garbage entries that would confuse M3's restore.
    if p.segments().next() == Some(TRASH_DIR) {
        return Err(Error::Unsupported);
    }
    // The basename can't be the sidecar's RESERVED name: a file called
    // `.norte-info` would make `payload == info` (same key) → the rename
    // would collide with the freshly written `.norte-info` (Conflict +
    // orphan). Cleanly rejected; the frontend degrades to permanent if the
    // user confirms.
    if basename.as_bytes() == INFO_NAME {
        return Err(Error::Unsupported);
    }
    let id_seg = Segment::new(id.as_bytes().to_vec()).map_err(|_| Error::InvalidPath)?;
    let dir = provider_root(p).join(seg_const(TRASH_DIR)).join(id_seg);
    let payload = dir.join(basename);
    let info = dir.join(seg_const(INFO_NAME));
    Ok(TrashPaths { dir, payload, info })
}

/// `p`'s connection root (same scheme+authority, no segments).
fn provider_root(p: &VPath) -> VPath {
    let mut r = p.clone();
    while let Some(parent) = r.parent() {
        r = parent;
    }
    r
}

/// A segment from the bytes of a module constant (`TRASH_DIR`,
/// `INFO_NAME`). Invariant: they're valid literals; a panic here is a bug
/// in this module, never user input.
fn seg_const(bytes: &[u8]) -> Segment {
    Segment::new(bytes.to_vec()).expect("trash constant is a valid Segment")
}

/// Restoration metadata for a trash entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrashInfo {
    /// Original path, rebuilt from its wire form.
    pub original: VPath,
    /// Deletion instant, ms since epoch.
    pub deleted_ms: u64,
}

/// Serializes the restoration metadata to a `.norte-info`'s bytes. The
/// path travels as [`VPath::to_wire`] (percent-encoded ASCII, lossless, no
/// newlines or ASCII control characters) → the result is line-safe
/// (always exactly 3 lines, the `path:` value never contains `\n`).
#[must_use]
pub fn info_encode(original: &VPath, deleted_ms: u64) -> Vec<u8> {
    format!(
        "{INFO_HEADER}\npath: {}\ndeleted-ms: {deleted_ms}\n",
        original.to_wire()
    )
    .into_bytes()
}

/// Parses a `.norte-info`'s content and **anchors** the original path to
/// the trash's connection.
///
/// `expected_root` is the root of the provider this trash lives in (same
/// scheme+authority as the connection). The guard is a SECURITY one: a
/// `.norte-info` in a shared share/bucket is attacker-controllable;
/// without anchoring, a restore (M3) would write the payload to ANOTHER
/// connection/host (`path: sftp://other-host/.ssh/authorized_keys`) —
/// confused deputy. Traversal (`.`/`..`/`%2F`/NUL) is already blocked by
/// [`VPath::parse`]; here the scheme/authority vector is closed. The
/// policy for overwriting an existing file WITHIN the same connection is
/// the restore's decision (reinforced confirmation), not this parser's.
///
/// # Errors
/// [`Error::InvalidPath`] if the content isn't UTF-8, is missing the
/// header or a field, has extra lines, the wire path doesn't parse, the
/// timestamp isn't a `u64`, or the original path doesn't belong to
/// `expected_root` (different scheme or authority).
pub fn info_decode(bytes: &[u8], expected_root: &VPath) -> Result<TrashInfo, Error> {
    let text = std::str::from_utf8(bytes).map_err(|_| Error::InvalidPath)?;
    let mut lines = text.lines();
    if lines.next() != Some(INFO_HEADER) {
        return Err(Error::InvalidPath);
    }
    let wire = lines
        .next()
        .and_then(|l| l.strip_prefix("path: "))
        .ok_or(Error::InvalidPath)?;
    let ms = lines
        .next()
        .and_then(|l| l.strip_prefix("deleted-ms: "))
        .ok_or(Error::InvalidPath)?;
    // Strict: nothing after the 3rd field. Rejects half-corrupt
    // `.norte-info` files or ones with extra injected lines.
    if lines.next().is_some() {
        return Err(Error::InvalidPath);
    }
    let original = VPath::parse(wire).map_err(|_| Error::InvalidPath)?;
    // Confused-deputy guard: the restored path MUST belong to the SAME
    // connection as the trash.
    if original.scheme() != expected_root.scheme()
        || original.authority() != expected_root.authority()
    {
        return Err(Error::InvalidPath);
    }
    let deleted_ms = ms.parse::<u64>().map_err(|_| Error::InvalidPath)?;
    Ok(TrashInfo {
        original,
        deleted_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trash_id_is_sortable_and_valid_segment() {
        assert_eq!(trash_id(1_726_000_000_123, 0), "1726000000123-0");
        // Sorts lexicographically the same as numerically for equal width.
        assert!(trash_id(1_726_000_000_123, 0) < trash_id(1_726_000_000_124, 0));
        // Always builds a valid Segment (no `/`, no NUL, not `.`/`..`).
        assert!(Segment::new(trash_id(1, 2).into_bytes()).is_ok());
    }

    #[test]
    fn trash_id_carries_ms_and_formats_its_segment() {
        // The engine generates the id ONCE (#99): it carries `deleted_ms`
        // for the `.norte-info` and formats to the SAME segment as `trash_id`.
        let id = TrashId::new(1_726_000_000_123, 5);
        assert_eq!(id.as_segment(), "1726000000123-5");
        assert_eq!(id.as_segment(), trash_id(1_726_000_000_123, 5));
        assert_eq!(id.deleted_ms(), 1_726_000_000_123);
        // The segment works for `plan` (valid Segment).
        let p = VPath::parse("sftp://host/x").unwrap();
        assert!(plan(&p, &id.as_segment()).is_ok());
    }

    #[test]
    fn plan_builds_entry_under_provider_root() {
        let p = VPath::parse("sftp://host/deep/nested/victim.txt").unwrap();
        let paths = plan(&p, "1726000000123-0").unwrap();
        assert_eq!(
            paths.dir.to_wire(),
            "sftp://host/.norte-trash/1726000000123-0"
        );
        assert_eq!(
            paths.payload.to_wire(),
            "sftp://host/.norte-trash/1726000000123-0/victim.txt"
        );
        assert_eq!(
            paths.info.to_wire(),
            "sftp://host/.norte-trash/1726000000123-0/.norte-info"
        );
    }

    #[test]
    fn plan_preserves_hostile_basename() {
        // Non-UTF8 final segment (0xFF 0xFE): the payload keeps its bytes.
        let p = VPath::parse("sftp://host/dir/%FF%FE").unwrap();
        let paths = plan(&p, "1-0").unwrap();
        assert_eq!(
            paths.payload.to_wire(),
            "sftp://host/.norte-trash/1-0/%FF%FE"
        );
    }

    #[test]
    fn plan_refuses_provider_root() {
        let root = VPath::parse("sftp://host/").unwrap();
        assert!(matches!(plan(&root, "1-0"), Err(Error::Unsupported)));
    }

    #[test]
    fn plan_rejects_bad_id() {
        let p = VPath::parse("sftp://host/x").unwrap();
        // An id with `/` isn't a valid Segment.
        assert!(matches!(plan(&p, "bad/id"), Err(Error::InvalidPath)));
    }

    #[test]
    fn plan_refuses_trashing_the_trash_itself() {
        // The trash itself is never trashed (self-reference).
        let dir = VPath::parse("sftp://host/.norte-trash").unwrap();
        assert!(matches!(plan(&dir, "1-0"), Err(Error::Unsupported)));
        // Nor anything already living inside it (re-trashing an entry).
        let inside = VPath::parse("sftp://host/.norte-trash/2-0/x").unwrap();
        assert!(matches!(plan(&inside, "3-0"), Err(Error::Unsupported)));
        // But a NESTED `.norte-trash` file (not at the root) is fine
        // (no collision: payload `<id>/.norte-trash` ≠ info `<id>/.norte-info`).
        let nested = VPath::parse("sftp://host/dir/.norte-trash").unwrap();
        assert!(plan(&nested, "4-0").is_ok());
    }

    #[test]
    fn plan_refuses_reserved_info_basename() {
        // A file called `.norte-info` would collide with the sidecar.
        let root_info = VPath::parse("sftp://host/.norte-info").unwrap();
        assert!(matches!(plan(&root_info, "1-0"), Err(Error::Unsupported)));
        // Also nested (the basename is what collides, not the position).
        let nested_info = VPath::parse("sftp://host/dir/.norte-info").unwrap();
        assert!(matches!(plan(&nested_info, "2-0"), Err(Error::Unsupported)));
    }

    #[test]
    fn seg_const_uses_valid_constants() {
        // Ties down `seg_const`'s `expect()` invariant (rule 6): the
        // module's constants are always valid segments.
        assert!(Segment::new(TRASH_DIR.to_vec()).is_ok());
        assert!(Segment::new(INFO_NAME.to_vec()).is_ok());
    }

    #[test]
    fn info_roundtrips_hostile_path() {
        // A path with a non-UTF8 byte (0xFF) AND a control newline byte
        // (0x0A) inside a segment: to_wire escapes them to %FF/%0A → line-safe.
        let root = VPath::parse("sftp://host/").unwrap();
        let p = VPath::parse("sftp://host/a/%FF/x%0Ay").unwrap();
        let bytes = info_encode(&p, 1_726_000_000_123);

        // Line-safe: exactly 3 lines, no newline inside the value.
        let text = std::str::from_utf8(&bytes).unwrap();
        assert_eq!(text.lines().count(), 3);

        let info = info_decode(&bytes, &root).unwrap();
        assert_eq!(info.original, p);
        assert_eq!(info.deleted_ms, 1_726_000_000_123);
    }

    #[test]
    fn info_decode_rejects_corrupt() {
        let root = VPath::parse("sftp://host/").unwrap();
        assert!(matches!(
            info_decode(b"garbage", &root),
            Err(Error::InvalidPath)
        ));
        assert!(matches!(
            info_decode(b"norte-trash-info v1\npath: sftp://host/x\n", &root),
            Err(Error::InvalidPath) // missing deleted-ms
        ));
        assert!(matches!(
            info_decode(
                b"norte-trash-info v1\npath: not-a-wire-path\ndeleted-ms: 5\n",
                &root
            ),
            Err(Error::InvalidPath) // wire doesn't parse
        ));
        assert!(matches!(
            info_decode(
                b"norte-trash-info v1\npath: sftp://host/x\ndeleted-ms: NaN\n",
                &root
            ),
            Err(Error::InvalidPath) // ms not numeric
        ));
    }

    #[test]
    fn info_decode_rejects_trailing_lines() {
        // Strict: an injected 4th line invalidates the file.
        let root = VPath::parse("sftp://host/").unwrap();
        assert!(matches!(
            info_decode(
                b"norte-trash-info v1\npath: sftp://host/x\ndeleted-ms: 5\ninjected\n",
                &root
            ),
            Err(Error::InvalidPath)
        ));
    }

    #[test]
    fn info_decode_anchors_to_connection() {
        // Confused-deputy guard: a poisoned .norte-info pointing at ANOTHER
        // connection (different scheme or authority) is rejected.
        let root = VPath::parse("sftp://host/").unwrap();

        // Different scheme (sftp trash → file:// path).
        assert!(matches!(
            info_decode(
                b"norte-trash-info v1\npath: file:///etc/passwd\ndeleted-ms: 0\n",
                &root
            ),
            Err(Error::InvalidPath)
        ));
        // Different authority (another host).
        assert!(matches!(
            info_decode(
                b"norte-trash-info v1\npath: sftp://evil/home/victim/.ssh/authorized_keys\ndeleted-ms: 0\n",
                &root
            ),
            Err(Error::InvalidPath)
        ));
        // Same connection (same scheme+authority): accepted, deep path ok.
        let ok = info_decode(
            b"norte-trash-info v1\npath: sftp://host/deep/nested/file.txt\ndeleted-ms: 7\n",
            &root,
        )
        .unwrap();
        assert_eq!(
            ok.original,
            VPath::parse("sftp://host/deep/nested/file.txt").unwrap()
        );
        assert_eq!(ok.deleted_ms, 7);
    }
}
