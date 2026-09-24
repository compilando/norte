//! [`ObjectProvider`]: the [`Provider`] trait over an [`opendal::Operator`]
//! (ADR 0016). Object storage has no directories: they are modeled as
//! marker objects (`key/`) + prefix probing, with file > dir precedence.

use async_trait::async_trait;
use bytes::Bytes;
use futures::{StreamExt, TryStreamExt};
use norte_proto::{
    Authority, ByteRange, Capabilities, CapabilityFlags, ConflictKind, Entry, EntryKind, Error,
    Scheme, Segment, VPath,
};
use norte_vfs::{ByteSink, ByteStream, EntryStream, Provider, trash};
use opendal::{ErrorKind, Metadata, Operator};

/// S3's limit for the FULL KEY's length: 1024 UTF-8 BYTES. The EFFECTIVE
/// budget for the provider's path is computed in [`new`] by subtracting the
/// `Operator`'s `root` prefix (which opendal prepends to every key before
/// sending it) — without that discount, an `Operator` with a long `root`
/// would let through keys the server rejects midway through an operation
/// with an ambiguous error. There is no per-segment limit (unlike a POSIX
/// FS's `NAME_MAX`): a 300-byte segment is a legal S3 key.
const MAX_KEY_BYTES: usize = 1024;

/// The multipart writer's chunk: 8 MiB (S3's minimum = 5 MiB; opendal
/// buffers up to this before uploading a part — smaller objects go through
/// `PutObject`).
const WRITE_CHUNK: usize = 8 * 1024 * 1024;

/// VFS provider over object storage (ADR 0016).
///
/// The [`Operator`] arrives ALREADY configured (bucket/region/endpoint/
/// credentials are resolved in `norte-connect`, phase 7d): this provider
/// never sees a secret. S3 semantics over the trait's contract:
///
/// - **Directories** = marker objects (`key/`) + prefix probing; file > dir
///   precedence (S3 lets `x` and `x/` coexist; from this provider it is
///   impossible to create because `write`/`mkdir` check each other).
/// - **UTF-8-only keys** (an S3 protocol limit, not the library's): a name
///   that cannot be represented is [`Error::InvalidPath`], rule 1.
/// - **`rename` is NOT atomic and is O(n)** on directories (copy-all then
///   delete-all: a mid-way failure leaves duplicates, never loss) — that is
///   why `RENAME_ATOMIC` is not declared.
/// - **Resume**: deferred (ADR 0016 F) — opendal 0.58 does not expose
///   resuming a multipart upload; `open_resumable` inherits the `(write, 0)`
///   default and cancelling leaves the destination clean (no remote
///   `.norte-partial`).
pub struct ObjectProvider {
    op: Operator,
    scheme: String,
    /// Bytes available for the provider's path = 1024 − the `Operator`'s
    /// `root` prefix − 1 (the dir variant adds a `/`). Computed in [`new`].
    key_budget: usize,
    /// The backend announces server-side `copy` (S3 does; another
    /// `Operator` might not). Gates `SERVER_COPY`: without it, declaring it
    /// would make the engine hard-fail a file (`Some(Err(Unsupported))`, no
    /// fallback to streaming) where streaming would have worked.
    server_copy: bool,
    /// Logical `.norte-trash/` trash active (per-connection opt-in, ADR
    /// 0019). Off by default → does not declare `TRASH` → permanent delete.
    logical_trash: bool,
}

impl ObjectProvider {
    /// Provider over an already-configured `Operator`, for `scheme` (`"s3"`).
    ///
    /// The `Operator`'s root (bucket + the builder's `root` prefix) is the
    /// provider's root: there is no `base` here — whoever builds it sets it.
    #[must_use]
    pub fn new(op: Operator, scheme: impl Into<String>) -> Self {
        // The Operator's `root` (e.g. `/team/project/`) is prepended to
        // every key BEFORE sending it to the server (the leading `/` does
        // not count — opendal trims it). It is deducted from the 1024
        // budget so as not to let through keys the server would reject
        // midway through an operation.
        let root_prefix = op.info().root().trim_start_matches('/').len();
        // −1: reserves the directory variant's trailing `/`.
        let key_budget = MAX_KEY_BYTES.saturating_sub(root_prefix).saturating_sub(1);
        let server_copy = op.info().capability().copy;
        Self {
            op,
            scheme: scheme.into(),
            key_budget,
            server_copy,
            logical_trash: false,
        }
    }

    /// Turns the logical `.norte-trash/` trash on/off (ADR 0019). Without
    /// it the provider does not declare `TRASH` and `trash()` gives
    /// `Unsupported`.
    #[must_use]
    pub fn with_logical_trash(mut self, enabled: bool) -> Self {
        self.logical_trash = enabled;
        self
    }

    /// Reads and validates a trash entry's `.norte-info`: `true` only if it
    /// decodes to exactly `p` (same victim). Distinguishes OUR entry from a
    /// foreign collision with the same id (#99, rust review MAJOR). Absent,
    /// unreadable or a different victim = `false`.
    async fn trash_info_matches(&self, info: &VPath, p: &VPath) -> bool {
        let Ok(mut stream) = self.read(info, None).await else {
            return false;
        };
        let mut buf = Vec::new();
        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(b) => buf.extend_from_slice(&b),
                Err(_) => return false,
            }
        }
        trash::info_decode(&buf, p).is_ok_and(|i| i.original == *p)
    }

    /// Creates the `dir` marker, tolerating that it already exists
    /// (idempotent): useful for `.norte-trash/` under cross-session
    /// concurrency.
    async fn ensure_dir_idempotent(&self, dir: &VPath) -> Result<(), Error> {
        match self.mkdir(dir).await {
            Ok(())
            | Err(Error::Conflict {
                conflict: ConflictKind::Exists,
            }) => Ok(()),
            Err(e) => match self.stat_kind(&self.key(dir)?).await? {
                Some((EntryKind::Dir, _)) => Ok(()),
                _ => Err(e),
            },
        }
    }

    /// This provider's root for a `scheme`/`authority` (`s3://bucket/`).
    ///
    /// # Panics
    /// If `scheme` is not a valid scheme (`[a-z][a-z0-9+.-]*`). Callers pass
    /// a constant (`"s3"`), so in practice this never happens.
    #[must_use]
    pub fn root(scheme: &str, authority: Authority) -> VPath {
        VPath::root(Scheme::new(scheme).expect("valid scheme"), Some(authority))
    }

    /// Translates a [`VPath`] into the object's key (no trailing `/`).
    /// Segments are BYTES; S3 keys are UTF-8 — a name that cannot be
    /// represented is [`Error::InvalidPath`] (a CLEAN rejection, rule 1, ADR
    /// 0016 D). The key is ALWAYS built this way, never from an echoed one.
    ///
    /// No CRLF filter (unlike FTP): S3 travels over signed HTTP (sigv4
    /// covers the path), there is no line protocol to inject into.
    fn key(&self, p: &VPath) -> Result<String, Error> {
        if p.scheme() != self.scheme {
            return Err(Error::InvalidPath);
        }
        let mut out = String::new();
        for seg in p.segments() {
            let name = std::str::from_utf8(seg).map_err(|_| Error::InvalidPath)?;
            if name.contains('/') || name == "." || name == ".." {
                return Err(Error::InvalidPath);
            }
            // U+FFFD TO WRITE: S3-legal (UTF-8), but `list` uses it as the
            // sentinel for "bytes lost in lossy decoding" and cuts the
            // listing short at it. Creating one would leave the parent
            // directory unlistable (self-DoS) — rejected here so write and
            // list stay symmetric (debt: distinguish a legitimate U+FFFD,
            // #37).
            if name.contains('\u{FFFD}') {
                return Err(Error::InvalidPath);
            }
            // opendal-core's `normalize_path` does `path.trim()`: a name
            // with leading/trailing Unicode whitespace would be SILENTLY
            // RENAMED ("file " → "file") on EVERY backend — byte corruption,
            // rule 1. Uniform fail-loud rejection per segment (S3 allows
            // them; upstream debt logged, issue #48).
            if name.trim() != name {
                return Err(Error::InvalidPath);
            }
            if !out.is_empty() {
                out.push('/');
            }
            out.push_str(name);
        }
        if out.len() > self.key_budget {
            return Err(Error::InvalidPath);
        }
        Ok(out)
    }

    /// The key in its DIRECTORY form (`key/`); the root is `""` (opendal
    /// lists the Operator's root with an empty path).
    fn dir_key(&self, p: &VPath) -> Result<String, Error> {
        let k = self.key(p)?;
        if k.is_empty() {
            Ok(k)
        } else {
            Ok(format!("{k}/"))
        }
    }

    /// Internal `stat`: file first, dir (marker or prefix with children)
    /// after. `None` = does not exist. The file > dir precedence is
    /// documented in ADR 0016 C.
    async fn stat_kind(&self, key: &str) -> Result<Option<(EntryKind, Metadata)>, Error> {
        match self.op.stat(key).await {
            // services-fs (the contract's harness) answers stat on a real
            // directory's key WITHOUT a slash with mode=DIR; S3 only does
            // so for files.
            Ok(m) if m.mode().is_dir() => return Ok(Some((EntryKind::Dir, m))),
            Ok(m) => return Ok(Some((EntryKind::File, m))),
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            // stat of a dir by its slash-less key: some backends flag this
            // instead of NotFound — falls through to the dir probe below.
            Err(e) if e.kind() == ErrorKind::IsADirectory => {}
            Err(e) => return Err(map_err(&e)),
        }
        // A dir? On S3 `stat("key/")` is opendal's CompleteLayer probe:
        // a marker OR a prefix with children (list limit 1); on fs, the
        // directory's real stat. Deliberate fs/S3 asymmetry: under a file
        // (`a.txt/child`) the fs harness gives TypeMismatch (ENOTDIR) and
        // S3 gives NotFound — the contract only exercises the former.
        match self.op.stat(&format!("{key}/")).await {
            Ok(m) => Ok(Some((EntryKind::Dir, m))),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
            Err(e) => Err(map_err(&e)),
        }
    }

    /// Does `p`'s parent exist as a directory? The root always exists.
    async fn parent_dir_exists(&self, p: &VPath) -> Result<bool, Error> {
        let Some(parent) = p.parent() else {
            // No parent = `p` is the root; its "parent" does not apply.
            return Ok(true);
        };
        if parent.parent().is_none() {
            return Ok(true); // the parent is the provider's root
        }
        let key = self.key(&parent)?;
        Ok(matches!(
            self.stat_kind(&key).await?,
            Some((EntryKind::Dir, _))
        ))
    }

    /// Checks the destination is free (neither file nor dir) → if not, `Conflict`.
    async fn ensure_absent(&self, key: &str) -> Result<(), Error> {
        if self.stat_kind(key).await?.is_some() {
            return Err(Error::Conflict {
                conflict: ConflictKind::Exists,
            });
        }
        Ok(())
    }
}

impl std::fmt::Debug for ObjectProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ObjectProvider")
            .field("scheme", &self.scheme)
            .finish_non_exhaustive()
    }
}

/// A listed entry's VALIDATED suffix relative to `from_dir` (#49): the
/// anti-lying-server containment for the prefix rename — `None` = the
/// listed dir itself (skipped); `Err(InvalidPath)` = the entry falls outside
/// the prefix or carries illegal components (lossy `�`, `.`/`..`/empty).
/// The copy/delete keys are ALWAYS REBUILT from this suffix, the server's
/// path is never echoed.
fn validated_suffix<'a>(from_dir: &str, path: &'a str) -> Result<Option<&'a str>, Error> {
    if path == from_dir {
        return Ok(None);
    }
    let Some(suffix) = path.strip_prefix(from_dir) else {
        return Err(Error::InvalidPath);
    };
    if suffix.contains('\u{FFFD}') {
        return Err(Error::InvalidPath);
    }
    // Every segment of the suffix (recursive → can carry `/`; a subdir ends
    // in `/`) must be a legal name.
    let trimmed = suffix.strip_suffix('/').unwrap_or(suffix);
    if trimmed.is_empty()
        || trimmed
            .split('/')
            .any(|c| c.is_empty() || c == "." || c == "..")
    {
        return Err(Error::InvalidPath);
    }
    Ok(Some(suffix))
}

/// Maps opendal's error onto the protocol's taxonomy (spec §17.7).
fn map_err(e: &opendal::Error) -> Error {
    match e.kind() {
        ErrorKind::NotFound => Error::NotFound,
        ErrorKind::PermissionDenied => Error::PermissionDenied,
        // Failed conditional write (If-None-Match): the destination appeared.
        ErrorKind::ConditionNotMatch | ErrorKind::AlreadyExists => Error::Conflict {
            conflict: ConflictKind::Exists,
        },
        ErrorKind::IsADirectory | ErrorKind::NotADirectory => Error::Conflict {
            conflict: ConflictKind::TypeMismatch,
        },
        ErrorKind::RateLimited => Error::Io { retryable: true },
        ErrorKind::Unsupported => Error::Unsupported,
        // `is_temporary`: this is how opendal flags network/service errors
        // that deserve a retry (the cancellable backoff is the engine's).
        _ if e.is_temporary() => Error::ProviderUnavailable { retryable: true },
        _ => Error::Io { retryable: false },
    }
}

/// The object provider's attr catalogue (#108 block 2).
///
/// `s3.etag` travels in `ListObjectsV2` AND in `HeadObject` (free in both);
/// `s3.content_type` ONLY arrives in stat (`HeadObject`) — in listings it is
/// absent, which is contract-legal ("absence means absence"; the UI's
/// hydration re-stats the focused entry). `s3.storage_class` is impossible
/// with opendal 0.58 (it discards it while parsing the XML) — debt with its
/// own issue at the block's close.
fn catalogo_s3() -> &'static [norte_proto::AttrInfo] {
    use norte_proto::{AttrHint, AttrInfo, AttrType};
    static CAT: std::sync::LazyLock<Vec<AttrInfo>> = std::sync::LazyLock::new(|| {
        let mk = |id: &str, label: &str| AttrInfo {
            id: id.to_owned(),
            label: label.to_owned(),
            ty: AttrType::Text,
            hint: AttrHint::Opaque,
        };
        vec![mk("s3.etag", "ETag"), mk("s3.content_type", "Content-Type")]
    });
    &CAT
}

/// Materializes the requested attrs from opendal's metadata. A value over
/// Text's cap is OMITTED (never silently truncated: an absent cell is
/// honest, a cropped one would lie).
fn attrs_from_meta(
    m: &Metadata,
    req: &norte_vfs::AttrRequest,
) -> std::collections::BTreeMap<String, norte_proto::AttrValue> {
    use norte_proto::AttrValue;
    let mut out = std::collections::BTreeMap::new();
    if req.is_empty() {
        return out;
    }
    let mut text = |id: &str, v: Option<&str>| {
        if let Some(s) = v
            && req.wants(id)
            && s.len() <= norte_proto::ATTR_TEXT_MAX
        {
            out.insert(id.to_owned(), AttrValue::Text(s.to_owned()));
        }
    };
    text("s3.etag", m.etag());
    text("s3.content_type", m.content_type());
    out
}

/// A file's `Entry` from opendal's metadata.
fn file_entry(path: VPath, m: &Metadata, req: &norte_vfs::AttrRequest) -> Entry {
    Entry {
        attrs: attrs_from_meta(m, req),
        path,
        kind: EntryKind::File,
        size: Some(m.content_length()),
        mtime_ms: m.last_modified().map(jiff_ms),
    }
}

fn jiff_ms(ts: opendal::raw::Timestamp) -> i64 {
    ts.into_inner().as_millisecond()
}

#[async_trait]
impl Provider for ObjectProvider {
    fn scheme(&self) -> &str {
        &self.scheme
    }

    fn capabilities(&self) -> Capabilities {
        // Honest ones (ADR 0016 H): byte-exact UTF-8 keys → case-sensitive
        // and case-preserving. SERVER_COPY = CopyObject (phase 7c, the
        // repo's first provider to implement it) ONLY if the backend
        // announces `copy` — without that gate, a backend without copy
        // would hard-fail a file that streaming would have copied. Does NOT
        // declare: APPEND/RANDOM_WRITE (S3 has neither), SYMLINKS,
        // RENAME_ATOMIC (O(n) copy+delete). TRASH only if the connection
        // turned on the logical `.norte-trash/` trash (ADR 0019):
        // relocation reuses the copy-all→delete-all rename.
        let mut flags = CapabilityFlags::CASE_SENSITIVE | CapabilityFlags::CASE_PRESERVING;
        if self.server_copy {
            flags |= CapabilityFlags::SERVER_COPY;
        }
        if self.logical_trash {
            flags |= CapabilityFlags::TRASH;
        }
        Capabilities {
            flags,
            // The EFFECTIVE budget (1024 − root prefix − 1), not S3's raw
            // limit: what `key()` accepts = what the core pre-validates.
            max_path: u32::try_from(self.key_budget).ok(),
        }
    }

    async fn stat(&self, p: &VPath) -> Result<Entry, Error> {
        self.stat_with(p, &norte_vfs::ListOptions::default()).await
    }

    fn attrs(&self) -> &[norte_proto::AttrInfo] {
        catalogo_s3()
    }

    async fn stat_with(&self, p: &VPath, opt: &norte_vfs::ListOptions) -> Result<Entry, Error> {
        let key = self.key(p)?;
        if key.is_empty() {
            // The provider's root (the bucket) always exists as a dir.
            return Ok(Entry {
                attrs: std::collections::BTreeMap::new(),
                path: p.clone(),
                kind: EntryKind::Dir,
                size: None,
                mtime_ms: None,
            });
        }
        match self.stat_kind(&key).await? {
            Some((EntryKind::File, m)) => Ok(file_entry(p.clone(), &m, &opt.attrs)),
            Some((_, _)) => Ok(Entry {
                attrs: std::collections::BTreeMap::new(),
                path: p.clone(),
                // A marker's mtime does not describe the "directory" (the
                // objects inside it change without touching it): an honest
                // None.
                kind: EntryKind::Dir,
                size: None,
                mtime_ms: None,
            }),
            None => Err(Error::NotFound),
        }
    }

    async fn list(&self, p: &VPath) -> Result<EntryStream, Error> {
        self.list_with(p, &norte_vfs::ListOptions::default()).await
    }

    async fn list_with(
        &self,
        p: &VPath,
        opt: &norte_vfs::ListOptions,
    ) -> Result<EntryStream, Error> {
        let dir = self.dir_key(p)?;
        if !dir.is_empty() {
            // NotFound / not-a-dir go in the Result, not as the first item.
            match self.stat_kind(self.key(p)?.as_str()).await? {
                Some((EntryKind::Dir, _)) => {}
                Some(_) => {
                    return Err(Error::Conflict {
                        conflict: ConflictKind::TypeMismatch,
                    });
                }
                None => return Err(Error::NotFound),
            }
        }
        let lister = self.op.lister(&dir).await.map_err(|e| map_err(&e))?;
        let base = p.clone();
        let self_key = dir;
        // Arc: the request is read-only and the closure clones PER ENTRY —
        // without Arc, every listed object would pay for a new Vec<String>.
        let req = std::sync::Arc::new(opt.attrs.clone());
        // LAZY stream: opendal paginates underneath with its own
        // ContinuationToken (the contact point with cursor pagination, ADR
        // 0017).
        let stream = lister.map_err(|e| map_err(&e)).try_filter_map(move |oe| {
            let base = base.clone();
            let self_key = self_key.clone();
            let req = std::sync::Arc::clone(&req);
            async move {
                let path = oe.path();
                // opendal returns the listed dir itself as an entry; when
                // listing the ROOT (empty self_key) it emits it as `/`.
                if path == self_key || (self_key.is_empty() && path == "/") {
                    return Ok(None);
                }
                let is_dir = oe.metadata().mode().is_dir();
                // The name MUST hang off the requested prefix. A lying
                // server echoing a key outside `self_key` (`other`, `../x`)
                // is NOT accepted with a fallback to the full path: it cuts
                // fail-loud (never a phantom Entry under `base`).
                let Some(rest) = path.strip_prefix(self_key.as_str()) else {
                    return Err(Error::InvalidPath);
                };
                let name = rest.trim_end_matches('/');
                // `""` (from a `dir//x` with an empty segment), `.` and
                // `..` are legal S3 keys the dir model cannot represent:
                // they are CUT (not silently hidden — they would be
                // invisible data in a list-driven migration).
                if name.is_empty() || name == "." || name == ".." {
                    return Err(Error::InvalidPath);
                }
                // A backend name with `/` (an unexpected sub-prefix, or a
                // `/` injected to escape the dir) or U+FFFD (original bytes
                // lost in a lossy decoding) cuts the listing: fail-loud
                // rejection, rule 1.
                if name.contains('/') || name.contains('\u{FFFD}') {
                    return Err(Error::InvalidPath);
                }
                let seg = Segment::new(name.as_bytes().to_vec()).map_err(|_| Error::InvalidPath)?;
                let child = base.join(seg);
                let entry = if is_dir {
                    Entry {
                        attrs: std::collections::BTreeMap::new(),
                        path: child,
                        kind: EntryKind::Dir,
                        size: None,
                        mtime_ms: None,
                    }
                } else {
                    file_entry(child, oe.metadata(), &req)
                };
                Ok(Some(entry))
            }
        });
        Ok(stream.boxed())
    }

    async fn read(&self, p: &VPath, range: Option<ByteRange>) -> Result<ByteStream, Error> {
        let key = self.key(p)?;
        // Rejects dirs (reading them is an error) and absent ones (NotFound).
        let size = match self.stat_kind(&key).await? {
            None => return Err(Error::NotFound),
            Some((EntryKind::Dir, _)) => {
                return Err(Error::Conflict {
                    conflict: ConflictKind::TypeMismatch,
                });
            }
            Some((_, m)) => m.content_length(),
        };
        // The trait's pread semantics: offset > EOF = empty stream and len
        // gets clamped to EOF. CLAMPED here with the stat's size (objects
        // are immutable) instead of trusting the server's 416: opendal's
        // reader demands an end within the file or it fails mid-stream.
        let (offset, len) = match range {
            Some(ByteRange { offset, len }) => (offset, len),
            None => (0, None),
        };
        let start = offset.min(size);
        let avail = size - start;
        let want = len.map_or(avail, |l| l.min(avail));
        if want == 0 {
            return Ok(futures::stream::empty().boxed());
        }
        let reader = self.op.reader(&key).await.map_err(|e| map_err(&e))?;
        match reader
            .into_bytes_stream(opendal::BytesRange::new(start, Some(want)))
            .await
        {
            Ok(s) => Ok(s
                .map_ok(Bytes::from)
                // A network drop MID-DOWNLOAD keeps the `retryable` flag
                // (the engine retries with backoff) — like local's
                // `map_io`; only the transport's transient ones.
                .map_err(|e| Error::Io {
                    retryable: matches!(
                        e.kind(),
                        std::io::ErrorKind::Interrupted
                            | std::io::ErrorKind::TimedOut
                            | std::io::ErrorKind::WouldBlock
                            | std::io::ErrorKind::ConnectionReset
                            | std::io::ErrorKind::ConnectionAborted
                            | std::io::ErrorKind::BrokenPipe
                    ),
                })
                .boxed()),
            // A belt for the clamp above (should not trigger with immutable
            // objects): 416 = empty stream, not an error.
            Err(e) if e.kind() == ErrorKind::RangeNotSatisfied => {
                Ok(futures::stream::empty().boxed())
            }
            Err(e) => Err(map_err(&e)),
        }
    }

    async fn write(&self, p: &VPath) -> Result<Box<dyn ByteSink>, Error> {
        let key = self.key(p)?;
        if key.is_empty() {
            return Err(Error::InvalidPath);
        }
        if !self.parent_dir_exists(p).await? {
            return Err(Error::NotFound);
        }
        // Create-new: the contract requires `Conflict` ON OPEN. This upfront
        // stat-check is ALWAYS kept as a belt (ADR 0016 E): against a server
        // that ignores If-None-Match the guarantee degrades to this check
        // (racy, ftp-level), never to an overwrite without one.
        self.ensure_absent(&key).await?;
        // The invisible staging is the multipart upload itself (or the
        // buffered PutObject): nothing exists at the key until `close()`.
        // If-None-Match travels in the commit → race-free create-new on
        // honest servers.
        let mut w = self.op.writer_with(&key).chunk(WRITE_CHUNK);
        if self.op.info().capability().write_with_if_not_exists {
            w = w.if_not_exists(true);
        }
        let writer = w.await.map_err(|e| map_err(&e))?;
        Ok(Box::new(ObjectSink {
            writer: Some(writer),
        }))
    }

    async fn mkdir(&self, p: &VPath) -> Result<(), Error> {
        let key = self.key(p)?;
        if key.is_empty() {
            // The root already exists.
            return Err(Error::Conflict {
                conflict: ConflictKind::Exists,
            });
        }
        if !self.parent_dir_exists(p).await? {
            return Err(Error::NotFound);
        }
        self.ensure_absent(&key).await?;
        // On S3 opendal's CompleteLayer writes the marker (empty `key/`);
        // on fs it is the real mkdir. Some backends' implicit `mkdir -p` is
        // harmless: the parent was already validated above.
        self.op
            .create_dir(&format!("{key}/"))
            .await
            .map_err(|e| map_err(&e))
    }

    async fn remove(&self, p: &VPath) -> Result<(), Error> {
        let key = self.key(p)?;
        if key.is_empty() {
            return Err(Error::InvalidPath);
        }
        // Prior stat: opendal's delete is idempotent and would lie about the
        // honest NotFound the contract requires.
        match self.stat_kind(&key).await? {
            None => Err(Error::NotFound),
            Some((EntryKind::File, _)) => self.op.delete(&key).await.map_err(|e| map_err(&e)),
            Some(_) => {
                let dir = format!("{key}/");
                // A dir with children is refused (remove is NOT recursive).
                // The marker itself does not count as a child.
                let mut lister = self.op.lister(&dir).await.map_err(|e| map_err(&e))?;
                while let Some(oe) = lister.try_next().await.map_err(|e| map_err(&e))? {
                    if oe.path() != dir {
                        // Dir with children, like local (`DirectoryNotEmpty`).
                        return Err(Error::Conflict {
                            conflict: ConflictKind::TypeMismatch,
                        });
                    }
                }
                self.op.delete(&dir).await.map_err(|e| map_err(&e))
            }
        }
    }

    async fn rename(&self, from: &VPath, to: &VPath) -> Result<(), Error> {
        let from_key = self.key(from)?;
        let to_key = self.key(to)?;
        if from_key.is_empty() || to_key.is_empty() {
            return Err(Error::InvalidPath);
        }
        // A dir CANNOT be renamed into its own subtree (`a` → `a/b`): S3
        // would "execute" it by relocating and deleting the source marker,
        // and the fs harness would fail midway through (ENOTEMPTY). Rejected
        // cleanly, like local's POSIX EINVAL.
        if to_key == from_key || to_key.starts_with(&format!("{from_key}/")) {
            return Err(Error::InvalidPath);
        }
        let src = self.stat_kind(&from_key).await?.ok_or(Error::NotFound)?;
        if !self.parent_dir_exists(to).await? {
            return Err(Error::NotFound);
        }
        // Free destination (file and dir): a documented racy check (S3 has
        // no rename; CopyObject would silently overwrite).
        self.ensure_absent(&to_key).await?;
        if src.0 == EntryKind::File {
            self.op
                .copy(&from_key, &to_key)
                .await
                .map_err(|e| map_err(&e))?;
            return self.op.delete(&from_key).await.map_err(|e| map_err(&e));
        }
        // Dir = a whole prefix: copy-all IN STREAMING (#49: without
        // materializing the prefix — O(dirs) memory peak, not O(objects))
        // and THEN delete with a re-list + a batched deleter. The invariant
        // is preserved: the copy phase DRAINS the whole lister before the
        // first delete — a mid-way failure leaves duplicates, never loss.
        //
        // Suffixes VALIDATED relative to `from_dir`, NEVER the echoed key: a
        // lying server could list keys outside the prefix and make
        // copy/delete operate on (and DELETE) outside the tree — same
        // containment as `list()`. That is why the delete does NOT use
        // `Operator::remove_all` (deletes whatever the server echoes, no
        // containment): it re-lists and REBUILDS every key from the
        // validated suffix, with opendal's `Deleter` batching underneath
        // (DeleteObjects on S3).
        let from_dir = format!("{from_key}/");
        let to_dir = format!("{to_key}/");
        // Phase 0 — streaming PRE-VALIDATION (a historical reviewer
        // BLOCKER: a hostile listing must cut with ZERO mutations): the
        // lister is drained validating every suffix WITHOUT copying
        // anything — O(1) memory, one extra LIST sweep (n/1000 requests)
        // against n copies. A hostile entry SLIPPED IN between this sweep
        // and the copy cuts midway through phase 1: leaves duplicates
        // (never loss) and never an echoed key.
        let mut lister = self
            .op
            .lister_with(&from_dir)
            .recursive(true)
            .await
            .map_err(|e| map_err(&e))?;
        while let Some(oe) = lister.try_next().await.map_err(|e| map_err(&e))? {
            let _ = validated_suffix(&from_dir, oe.path())?;
        }
        // Phase 1 — streaming copy (dirs = create_dir at the destination;
        // services-fs's recursive mode includes subdirs, S3's markers as
        // objects with mode DIR — both go through create_dir).
        let mut lister = self
            .op
            .lister_with(&from_dir)
            .recursive(true)
            .await
            .map_err(|e| map_err(&e))?;
        while let Some(oe) = lister.try_next().await.map_err(|e| map_err(&e))? {
            let Some(suffix) = validated_suffix(&from_dir, oe.path())? else {
                continue; // the listed dir itself
            };
            if oe.metadata().mode().is_dir() {
                self.op
                    .create_dir(&format!("{to_dir}{suffix}"))
                    .await
                    .map_err(|e| map_err(&e))?;
            } else {
                self.op
                    .copy(&format!("{from_dir}{suffix}"), &format!("{to_dir}{suffix}"))
                    .await
                    .map_err(|e| map_err(&e))?;
            }
        }
        self.op.create_dir(&to_dir).await.map_err(|e| map_err(&e))?;
        // Phase 2 — delete with a re-list: files first (batched), dirs
        // after in reverse depth (an fs dir only deletes empty; O(dirs) in
        // memory, the tiny fraction of the tree).
        //
        // Slip-in guard: a file is only deleted if its DESTINATION exists —
        // a NEW key slipped in by another client between the two phases was
        // not copied and stays UNMOVED at the source. Watch the overclaim
        // (review #49 MAJOR-2): a slipped-in OVERWRITE of an ALREADY-copied
        // key IS lost (the v1 dst exists → the v2 src gets deleted) —
        // inherent to a non-atomic rename; a conditional delete by ETag
        // would be the fine-grained fix where the backend supports it.
        //
        // Request budget (documented on purpose): n sequential HEADs +
        // n/1000 DeleteObjects + 3 LIST sweeps. The per-file HEAD is the
        // guard's price; #49's gain is O(dirs) MEMORY and the delete's
        // batching, not fewer round-trips.
        let mut lister = self
            .op
            .lister_with(&from_dir)
            .recursive(true)
            .await
            .map_err(|e| map_err(&e))?;
        // A `?` (or dropping the future, rule 3) drops the `deleter`
        // without close(): deletes queued without a flush are DISCARDED —
        // they stay as duplicates at the source, never a loss (the usual
        // invariant).
        let mut deleter = self.op.deleter().await.map_err(|e| map_err(&e))?;
        let mut dirs: Vec<String> = Vec::new();
        while let Some(oe) = lister.try_next().await.map_err(|e| map_err(&e))? {
            let Some(suffix) = validated_suffix(&from_dir, oe.path())? else {
                continue;
            };
            if oe.metadata().mode().is_dir() {
                dirs.push(suffix.to_owned());
                continue;
            }
            let dst = format!("{to_dir}{suffix}");
            if !self.op.exists(&dst).await.map_err(|e| map_err(&e))? {
                // escape_debug: the suffix is legal but can carry
                // controls/ANSI — never raw to the log (the redacted-VPath
                // convention).
                tracing::warn!(
                    suffix = %suffix.escape_debug(),
                    "object slipped in during the prefix rename: stays UNMOVED at the source"
                );
                continue;
            }
            deleter
                .delete(format!("{from_dir}{suffix}"))
                .await
                .map_err(|e| map_err(&e))?;
        }
        // Flush the files' batch BEFORE touching dirs (fs demands empty).
        deleter.close().await.map_err(|e| map_err(&e))?;
        dirs.sort_by_key(|s| std::cmp::Reverse(s.len()));
        for suffix in dirs {
            self.op
                .delete(&format!("{from_dir}{suffix}"))
                .await
                .map_err(|e| map_err(&e))?;
        }
        self.op.delete(&from_dir).await.map_err(|e| map_err(&e))
    }

    async fn trash(&self, p: &VPath, id: &trash::TrashId) -> Result<Option<VPath>, Error> {
        if !self.logical_trash {
            return Err(Error::Unsupported);
        }
        // A DETERMINISTIC entry from the engine's id (#99). `plan` validates
        // `p` (rejects trashing the trash itself, ADR 0019) and gives the root.
        let paths = trash::plan(p, &id.as_segment())?;
        let trash_root = paths.dir.parent().ok_or(Error::Unsupported)?;

        // Idempotency: victim absent + deterministic payload present = this
        // op already applied in an earlier transient attempt → returns the
        // payload (recovers the `reversal_ref`). No payload = genuine
        // `NotFound`.
        match self.stat(p).await {
            Ok(_) => {}
            Err(Error::NotFound) => {
                return match self.stat(&paths.payload).await {
                    // Only OUR entry (the `.norte-info` decodes to `p`) is
                    // claimed; a foreign one with the same id is a collision
                    // (rust review MAJOR).
                    Ok(_) if self.trash_info_matches(&paths.info, p).await => {
                        Ok(Some(paths.payload))
                    }
                    Ok(_) => Err(Error::Conflict {
                        conflict: ConflictKind::Exists,
                    }),
                    Err(Error::NotFound) => Err(Error::NotFound),
                    Err(e) => Err(e),
                };
            }
            Err(e) => return Err(e),
        }

        self.ensure_dir_idempotent(&trash_root).await?;

        // The `<id>/` entry: `Conflict::Exists` is OUR partial (info absent
        // or decodes to `p`) → continue; a FOREIGN info with the same id is
        // a REAL collision (fixed id) → propagated without overwriting its
        // metadata (rust review MAJOR).
        match self.mkdir(&paths.dir).await {
            Ok(()) => {}
            Err(Error::Conflict {
                conflict: ConflictKind::Exists,
            }) => match self.stat(&paths.info).await {
                Err(Error::NotFound) => {}
                Ok(_) if self.trash_info_matches(&paths.info, p).await => {}
                Ok(_) => {
                    return Err(Error::Conflict {
                        conflict: ConflictKind::Exists,
                    });
                }
                Err(e) => return Err(e),
            },
            Err(e) => return Err(e),
        }

        // `.norte-info` BEFORE moving: if the rename fails, the source stays
        // intact or recoverable (copied to the trash), never a payload
        // without metadata. `deleted_ms` from the id (stable across retries).
        let info = trash::info_encode(p, id.deleted_ms());
        let mut sink = self.write(&paths.info).await?;
        sink.write(Bytes::from(info)).await?;
        sink.commit().await?;

        // Moves the tree, reusing the AUDITED rename: copy-all →
        // delete-all, keys rebuilt from validated suffixes (hostile-server
        // containment), no loss on interruption (ADR 0019/0016).
        self.rename(p, &paths.payload).await?;
        // LOGICAL trash: the payload IS the recoverable path → reversal_ref.
        Ok(Some(paths.payload))
    }

    /// The logical trash chooses its destination
    /// (`.norte-trash/<id>/payload`), so it always names it; without it
    /// there is no trash to promise.
    ///
    /// Without this, an S3 destination with logical trash returned
    /// `Some(dest)` while the trait's default said it did not know how to
    /// name it: the plan marked IRREVERSIBLE up to the last copy and the
    /// executor THREW AWAY a `reversal_ref` that existed (encoding-auditor
    /// MAJOR-4).
    fn trash_restorable(&self) -> bool {
        self.logical_trash
    }

    async fn copy_native(&self, from: &VPath, to: &VPath) -> Option<Result<(), Error>> {
        // Object storage DOES have server-side copy (CopyObject) — the
        // engine prefers it to read+rewrite. Applies to only ONE object
        // (file); a tree's copy is orchestrated by the engine with
        // list+copy_native per leaf. `Some(_)`: the engine only calls here
        // with SERVER_COPY and src==dst (same provider by pointer), so
        // native copy always applies; an error is propagated as-is (the
        // engine does NOT fall back to streaming).
        Some(self.copy_object(from, to).await)
    }
}

impl ObjectProvider {
    /// A file's `CopyObject` (ADR 0016 G). Existing destination →
    /// `Conflict` (never a silent overwrite, same policy as `write`). The
    /// prior `ensure_absent` is NOT just a belt: `If-None-Match: *` on the
    /// `to` key does NOT see a destination DIRECTORY (neither a `to/`
    /// marker nor a prefix with children), so `stat_kind`'s file+dir probe
    /// is the ONLY guard against copying a `to` file that aliases the `to/`
    /// dir. For a FILE destination: with `copy_with_if_not_exists` it is
    /// race-free; if the backend does not support it, it degrades to that
    /// check (racy, the same accepted level as write/ftp).
    async fn copy_object(&self, from: &VPath, to: &VPath) -> Result<(), Error> {
        let from_key = self.key(from)?;
        let to_key = self.key(to)?;
        if from_key.is_empty() || to_key.is_empty() {
            return Err(Error::InvalidPath);
        }
        // copy_native is single-object: a source directory is TypeMismatch
        // (the engine copies trees leaf by leaf, never passes a dir here).
        match self.stat_kind(&from_key).await? {
            None => return Err(Error::NotFound),
            Some((EntryKind::Dir, _)) => {
                return Err(Error::Conflict {
                    conflict: ConflictKind::TypeMismatch,
                });
            }
            Some(_) => {}
        }
        if !self.parent_dir_exists(to).await? {
            return Err(Error::NotFound);
        }
        // Belt: the upfront stat-check covers backends that ignore
        // If-None-Match (degrades to a racy level, never to an overwrite).
        self.ensure_absent(&to_key).await?;
        if self.op.info().capability().copy_with_if_not_exists {
            self.op
                .copy_with(&from_key, &to_key)
                .if_not_exists(true)
                .await
                .map(|_| ())
                .map_err(|e| map_err(&e))
        } else {
            self.op
                .copy(&from_key, &to_key)
                .await
                .map(|_| ())
                .map_err(|e| map_err(&e))
        }
    }
}

/// A write sink over object storage (ADR 0016 E). The invisible staging is
/// opendal's own multipart upload: parts go up in `write` (buffered per
/// chunk) and NOTHING exists at the final key until the `commit`'s
/// `CompleteMultipartUpload`/`PutObject`. `abort` =
/// `AbortMultipartUpload` (or discarding the buffer). No remote
/// `.norte-partial`; `keep` inherits the trait's default (= abort):
/// multipart resume is deferred (ADR 0016 F).
///
/// The `writer` is `Option` so [`Drop`] can EXTRACT it and abort the
/// orphaned multipart: a writer dropped without `close`/`abort` leaves the
/// already-uploaded parts hanging in the bucket (invisible but billable)
/// and this provider has no resume to find them again — `ByteSink`'s
/// contract requires best-effort cleanup in `Drop`.
struct ObjectSink {
    writer: Option<opendal::Writer>,
}

#[async_trait]
impl ByteSink for ObjectSink {
    async fn write(&mut self, chunk: Bytes) -> Result<(), Error> {
        if chunk.is_empty() {
            return Ok(());
        }
        let w = self.writer.as_mut().ok_or(Error::Io { retryable: false })?;
        w.write(chunk).await.map_err(|e| map_err(&e))
    }

    async fn commit(mut self: Box<Self>) -> Result<(), Error> {
        // If-None-Match travels here: `ConditionNotMatch` → Conflict (the
        // destination appeared between the open's stat-check and this commit).
        let mut w = self.writer.take().ok_or(Error::Io { retryable: false })?;
        w.close().await.map(|_| ()).map_err(|e| map_err(&e))
    }

    async fn abort(mut self: Box<Self>) -> Result<(), Error> {
        let mut w = self.writer.take().ok_or(Error::Io { retryable: false })?;
        w.abort().await.map_err(|e| map_err(&e))
    }
}

impl Drop for ObjectSink {
    fn drop(&mut self) {
        // ByteSink's contract: dropping without commit/abort = best-effort
        // abort. The multipart is not a synchronous unlink (like local) but
        // a network call — it is spawned on the current runtime if there is
        // one; without a runtime (a drop outside tokio) the parts are left
        // for the bucket's `AbortIncompleteMultipartUpload` lifecycle rule
        // (recommended in 7d).
        if let Some(mut w) = self.writer.take()
            && let Ok(handle) = tokio::runtime::Handle::try_current()
        {
            handle.spawn(async move {
                let _ = w.abort().await;
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::validated_suffix;
    use norte_proto::Error;

    /// #49: the rename's containment, case by case — the helper is the only
    /// gate through which an echoed path becomes an operation key.
    #[test]
    fn validated_suffix_contiene_lo_hostil() {
        let d = "src/";
        // The listed dir itself: skipped, not an error.
        assert_eq!(validated_suffix(d, "src/"), Ok(None));
        // Legal: file, subdir (with a trailing `/`), nested.
        assert_eq!(validated_suffix(d, "src/a.txt"), Ok(Some("a.txt")));
        assert_eq!(validated_suffix(d, "src/sub/"), Ok(Some("sub/")));
        assert_eq!(validated_suffix(d, "src/sub/b"), Ok(Some("sub/b")));
        // Hostile: outside the prefix, absolute, traversal, empty segment,
        // lossy.
        for hostile in [
            "otra/x",
            "/etc/passwd",
            "src/../victima",
            "src/a/../b",
            "src//oculto",
            "src/./x",
            "src/caf\u{FFFD}.txt",
            "src",
        ] {
            assert_eq!(
                validated_suffix(d, hostile),
                Err(Error::InvalidPath),
                "{hostile}"
            );
        }
    }
}
