//! [`SftpProvider`]: the [`Provider`] trait over an `SftpSession` from
//! `russh-sftp` (ADR 0013). Includes containment of a hostile server.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use bytes::Bytes;
use futures::StreamExt;
use norte_proto::{
    ByteRange, Capabilities, CapabilityFlags, ConflictKind, Entry, EntryKind, Error, Scheme, VPath,
};
use norte_vfs::{
    ByteSink, ByteStream, EntryStream, FollowLinks, NodeId, Provider, SymlinkKind, trash,
};
use russh_sftp::client::SftpSession;
use russh_sftp::protocol::OpenFlags;
use tokio::io::{AsyncSeekExt, AsyncWriteExt};

/// Read chunk size (256 KiB, aligned with the copy engine).
const READ_CHUNK: usize = 256 * 1024;
/// Prefix for the write staging (ADR 0012, same convention as local).
const PARTIAL_PREFIX: &str = ".norte-partial.";

/// VFS provider over an already-established SFTP session (ADR 0013).
///
/// The SSH connection with auth and host key verification lands in phase 6;
/// here the session is INJECTED ([`SftpProvider::new`]) — this way the
/// provider is tested against an in-process sftp server over `duplex`,
/// without SSH.
pub struct SftpProvider {
    session: Arc<SftpSession>,
    /// Absolute remote (POSIX) root everything lives under. No `..`.
    base: String,
    /// Staging counter (unique ephemeral name for `write`).
    seq: AtomicU64,
    /// Logical `.norte-trash/` trash active (opt-in per connection, ADR
    /// 0019). Off by default → does not declare `TRASH` → permanent delete.
    logical_trash: bool,
}

impl SftpProvider {
    /// Provider over an already-established session, rooted at `base`
    /// (absolute POSIX remote path, e.g. `/home/user`). `base` is
    /// normalized to have no trailing slash.
    #[must_use]
    pub fn new(session: SftpSession, base: impl Into<String>) -> Self {
        let mut base = base.into();
        while base.len() > 1 && base.ends_with('/') {
            base.pop();
        }
        Self {
            session: Arc::new(session),
            base,
            seq: AtomicU64::new(0),
            logical_trash: false,
        }
    }

    /// Enables/disables the logical `.norte-trash/` trash (ADR 0019).
    /// Without it the provider does not declare `TRASH` and `trash()` gives
    /// `Unsupported`.
    #[must_use]
    pub fn with_logical_trash(mut self, enabled: bool) -> Self {
        self.logical_trash = enabled;
        self
    }

    /// Reads and validates the `.norte-info` of a trash entry: `true` only if
    /// it decodes to exactly `p` (same victim). Distinguishes OUR entry from
    /// someone else's collision with the same id (#99, review rust MAJOR).
    /// Absent, unreadable or from another victim = `false`.
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

    /// Creates `dir` tolerating that it already exists (idempotent). Under
    /// v3 concurrency it can return a generic `Failure` (→ `Io`) instead of
    /// `Conflict` if another session creates it between the `exists()` and
    /// the `create_dir`; if in the end the directory is there, the result is
    /// benign.
    async fn ensure_dir_idempotent(&self, dir: &VPath) -> Result<(), Error> {
        match self.mkdir(dir).await {
            Ok(())
            | Err(Error::Conflict {
                conflict: ConflictKind::Exists,
            }) => Ok(()),
            Err(e) => {
                let remote = self.remote(dir)?;
                if self.exists(&remote).await.unwrap_or(false) {
                    Ok(())
                } else {
                    Err(e)
                }
            }
        }
    }

    /// This provider's root for a given `authority` (`sftp://host:22/`).
    ///
    /// # Panics
    /// Never: the scheme is constant and valid.
    #[must_use]
    pub fn root(authority: norte_proto::Authority) -> VPath {
        VPath::root(
            Scheme::new("sftp").expect("constant, valid scheme"),
            Some(authority),
        )
    }

    /// Translates a [`VPath`] into the absolute remote POSIX path under
    /// `base`. Segments are BYTES; SFTP (via russh-sftp) requires UTF-8 — a
    /// non-representable name is [`Error::InvalidPath`] (CLEAN rejection,
    /// never lossy — rule 1, ADR 0013 D2). The provider ALWAYS builds the
    /// path this way, never from a path echoed back by the server.
    fn remote(&self, p: &VPath) -> Result<String, Error> {
        if p.scheme() != "sftp" {
            return Err(Error::InvalidPath);
        }
        let mut out = String::from(&self.base);
        for seg in p.segments() {
            let name = std::str::from_utf8(seg).map_err(|_| Error::InvalidPath)?;
            // A segment never carries a separator nor is `.`/`..` (VPath
            // already guarantees it); defense in depth just in case.
            if name.contains('/') || name == "." || name == ".." {
                return Err(Error::InvalidPath);
            }
            if out.len() > 1 || !out.ends_with('/') {
                out.push('/');
            }
            out.push_str(name);
        }
        Ok(out)
    }

    /// Path of the stable resume staging for `p` (ADR 0012).
    fn stable_partial(&self, p: &VPath) -> Result<String, Error> {
        let parent = self.remote_parent(p)?;
        let name = p.file_name().ok_or(Error::InvalidPath)?;
        // Hash of the final name's bytes (SHA is not needed here: the test
        // server is trusted; the name only needs to be stable and unique per
        // destination — the same scheme as the ephemeral one is used).
        let hash = fnv1a_128(name.as_bytes());
        Ok(format!("{parent}/{PARTIAL_PREFIX}{hash:032x}"))
    }

    /// The remote path of `p`'s parent DIRECTORY.
    fn remote_parent(&self, p: &VPath) -> Result<String, Error> {
        let parent = p.parent().ok_or(Error::InvalidPath)?;
        self.remote(&parent)
    }

    fn session(&self) -> Arc<SftpSession> {
        Arc::clone(&self.session)
    }
}

impl std::fmt::Debug for SftpProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SftpProvider")
            .field("base", &self.base)
            .finish_non_exhaustive()
    }
}

/// 128-bit FNV-1a: stable hash (does not depend on the Rust version) for
/// naming staging. Not crypto — the test sftp server is trusted and the hash
/// only needs to be stable and unique per destination.
fn fnv1a_128(bytes: &[u8]) -> u128 {
    const OFFSET: u128 = 0x6c62_272e_07bb_0142_62b8_2175_6295_c58d;
    const PRIME: u128 = 0x0000_0000_0100_0000_0000_0000_0000_013b;
    let mut h = OFFSET;
    for &b in bytes {
        h ^= u128::from(b);
        h = h.wrapping_mul(PRIME);
    }
    h
}

/// Maps a russh-sftp error to the protocol's taxonomy (spec §17.7):
/// frontends render by category, never parse strings.
fn map_err(e: &russh_sftp::client::error::Error) -> Error {
    use russh_sftp::client::error::Error as E;
    use russh_sftp::protocol::StatusCode as S;
    match e {
        E::Status(st) => match st.status_code {
            // Eof when requesting metadata/reading a nonexistent one = NotFound.
            S::NoSuchFile | S::Eof => Error::NotFound,
            S::PermissionDenied => Error::PermissionDenied,
            S::OpUnsupported => Error::Unsupported,
            // v3 returns a generic `Failure` for almost everything (including
            // "already exists" in mkdir/rename): the caller who knows the
            // context reinterprets it; by default, non-retryable I/O.
            _ => Error::Io { retryable: false },
        },
        // Transport failure: the provider "is not responding" — retryable.
        E::IO(_) | E::Timeout | E::Limited(_) => Error::ProviderUnavailable { retryable: true },
        E::UnexpectedPacket | E::UnexpectedBehavior(_) => Error::Io { retryable: false },
    }
}

/// Reconstructs an [`Entry`] from sftp's `Metadata` over the requested
/// [`VPath`] (the authority/scheme are preserved — the path's identity on
/// the wire does not change by going through the provider).
/// Does `name` have the exact SHAPE of an sftp staging file? (#11) Narrow to
/// this provider's two forms — never the bare prefix (H2):
/// - stable: prefix + exactly 32 hex ([`SftpProvider`] resumable)
/// - ephemeral: prefix + `eph.` + digits (`write`'s counter)
fn is_norte_partial(name: &str) -> bool {
    let Some(rest) = name.strip_prefix(PARTIAL_PREFIX) else {
        return false;
    };
    let is_hex = |b: u8| b.is_ascii_digit() || (b'a'..=b'f').contains(&b);
    if rest.len() == 32 && rest.bytes().all(is_hex) {
        return true;
    }
    rest.strip_prefix("eph.")
        .is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
}

/// Attribute catalogue of the sftp provider (#108 block 2): what v3's
/// `SSH_FXP_ATTRS` ALREADY brings parsed — zero extra round-trips.
///
/// NOTE (debt): `sftp.owner`/`sftp.group` as `Bytes` require the raw longname
/// from `SSH_FXP_NAME`; russh-sftp 2.3 discards it before its client API and
/// ALWAYS decodes `user`/`group` to `None` in v3 (the wire only carries
/// uid/gid). Adjacent to #37 — its own issue when the block closes.
fn sftp_catalogue() -> &'static [norte_proto::AttrInfo] {
    use norte_proto::{AttrHint, AttrInfo, AttrType};
    static CAT: std::sync::LazyLock<Vec<AttrInfo>> = std::sync::LazyLock::new(|| {
        let mk = |id: &str, label: &str, hint| AttrInfo {
            id: id.to_owned(),
            label: label.to_owned(),
            ty: AttrType::Uint,
            hint,
        };
        vec![
            mk("posix.mode", "Mode", AttrHint::Mode),
            mk("posix.uid", "UID", AttrHint::Identity),
            mk("posix.gid", "GID", AttrHint::Identity),
        ]
    });
    &CAT
}

fn entry_from(
    path: VPath,
    md: &russh_sftp::protocol::FileAttributes,
    req: &norte_vfs::AttrRequest,
) -> Entry {
    let kind = if md.is_symlink() {
        EntryKind::Symlink
    } else if md.is_dir() {
        EntryKind::Dir
    } else if md.is_regular() {
        EntryKind::File
    } else {
        EntryKind::Other
    };
    let size = (kind == EntryKind::File).then(|| md.len());
    // sftp v3's mtime is u32 seconds since epoch.
    let mtime_ms = md.mtime.map(|s| i64::from(s) * 1000);
    // Absence means absence: a field the server did not send is omitted,
    // never faked as a 0 (#108 block 2).
    let mut attrs = std::collections::BTreeMap::new();
    if let Some(perm) = md.permissions
        && req.wants("posix.mode")
    {
        attrs.insert(
            "posix.mode".to_owned(),
            norte_proto::AttrValue::Uint(u64::from(perm)),
        );
    }
    if let Some(uid) = md.uid
        && req.wants("posix.uid")
    {
        attrs.insert(
            "posix.uid".to_owned(),
            norte_proto::AttrValue::Uint(u64::from(uid)),
        );
    }
    if let Some(gid) = md.gid
        && req.wants("posix.gid")
    {
        attrs.insert(
            "posix.gid".to_owned(),
            norte_proto::AttrValue::Uint(u64::from(gid)),
        );
    }
    Entry {
        attrs,
        path,
        kind,
        size,
        mtime_ms,
    }
}

#[async_trait]
impl Provider for SftpProvider {
    #[expect(
        clippy::unnecessary_literal_bound,
        reason = "the trait's signature is `-> &str`"
    )]
    fn scheme(&self) -> &str {
        "sftp"
    }

    fn capabilities(&self) -> Capabilities {
        // Honest (ADR 0013): sftp has symlinks and offset/append writes
        // (enabling the ADR 0012 resume), and a POSIX case-sensitive remote
        // is assumed. It does NOT declare: atomic rename (v3 does not
        // guarantee it) nor server-copy. TRASH only if the connection
        // enabled the logical `.norte-trash/` trash (ADR 0019).
        let mut flags = CapabilityFlags::SYMLINKS
            | CapabilityFlags::APPEND
            | CapabilityFlags::RANDOM_WRITE
            | CapabilityFlags::CASE_PRESERVING
            // The remote is assumed POSIX (case-sensitive): declaring it
            // keeps the engine from inventing case collisions that a Linux
            // server does not have (exact bytes = the conservative correct
            // choice).
            | CapabilityFlags::CASE_SENSITIVE
            // POSIX permissions (#314): `SSH_FXP_SETSTAT` with the
            // permissions field, which is what `chmod` does over sftp. The
            // remote is assumed POSIX as above; if it were not, the server
            // rejects it and that arrives as the error it is.
            | CapabilityFlags::POSIX_MODE;
        if self.logical_trash {
            flags |= CapabilityFlags::TRASH;
        }
        Capabilities {
            flags,
            max_path: None,
        }
    }

    async fn stat(&self, p: &VPath) -> Result<Entry, Error> {
        self.stat_with(p, &norte_vfs::ListOptions::default()).await
    }

    async fn stat_with(&self, p: &VPath, opt: &norte_vfs::ListOptions) -> Result<Entry, Error> {
        let remote = self.remote(p)?;
        // lstat: describes the LINK, never follows it (containment of trap
        // symlinks — ADR 0013).
        let md = self
            .session
            .symlink_metadata(remote)
            .await
            .map_err(|e| map_err(&e))?;
        Ok(entry_from(p.clone(), &md, &opt.attrs))
    }

    fn attrs(&self) -> &[norte_proto::AttrInfo] {
        sftp_catalogue()
    }

    async fn list(&self, p: &VPath) -> Result<EntryStream, Error> {
        self.list_with(p, &norte_vfs::ListOptions::default()).await
    }

    async fn list_with(
        &self,
        p: &VPath,
        opt: &norte_vfs::ListOptions,
    ) -> Result<EntryStream, Error> {
        let remote = self.remote(p)?;
        let base = p.clone();
        let dir = self
            .session
            .read_dir(remote)
            .await
            .map_err(|e| map_err(&e))?;
        let mut entries: Vec<Result<Entry, Error>> = Vec::new();
        for dent in dir {
            let name = dent.file_name();
            // `.`/`..` are not children; a name with `/` is a hostile server
            // trying to escape the base — it is rejected, the listing does
            // NOT blindly continue (ADR 0013).
            if name == "." || name == ".." {
                continue;
            }
            // russh-sftp decodes the server's names with `from_utf8_lossy`: a
            // non-UTF8 byte arrives already substituted by U+FFFD and the
            // original bytes were lost BELOW our boundary. Byte identity
            // cannot be guaranteed → CLEAN rejection instead of emitting a
            // corrupt Entry (rule 1 / ADR 0013 D2). Debt: read the raw bytes
            // of the SSH_FXP_NAME packet (issue #37).
            if name.contains('\u{FFFD}') || name.contains('/') {
                entries.push(Err(Error::InvalidPath));
                break;
            }
            let Ok(seg) = norte_proto::Segment::new(name.into_bytes()) else {
                entries.push(Err(Error::InvalidPath));
                break;
            };
            let child = base.join(seg);
            entries.push(Ok(entry_from(child, &dent.metadata(), &opt.attrs)));
        }
        Ok(futures::stream::iter(entries).boxed())
    }

    async fn read(&self, p: &VPath, range: Option<ByteRange>) -> Result<ByteStream, Error> {
        let remote = self.remote(p)?;
        let session = self.session();
        // Rejects dirs (reading them is an error, like the other providers).
        let md = session
            .symlink_metadata(&remote)
            .await
            .map_err(|e| map_err(&e))?;
        if md.is_dir() || md.is_symlink() {
            // A dir is not read; a symlink is NOT followed (consistent with
            // the lstat invariant of stat/node_id — ADR 0013). The engine
            // walks symlinks via read_link, never via read().
            return Err(Error::Conflict {
                conflict: ConflictKind::TypeMismatch,
            });
        }
        let mut file = session
            .open_with_flags(&remote, OpenFlags::READ)
            .await
            .map_err(|e| map_err(&e))?;
        if let Some(r) = range {
            file.seek(std::io::SeekFrom::Start(r.offset))
                .await
                .map_err(|_| Error::Io { retryable: false })?;
        }
        let len = range.and_then(|r| r.len);
        Ok(read_stream::sftp_read_stream(file, len))
    }

    async fn write(&self, p: &VPath) -> Result<Box<dyn ByteSink>, Error> {
        // Ephemeral staging: short unique name (not derived from the final
        // name, which may brush against NAME_MAX; ADR 0012). Contract: the
        // final destination must NOT exist (create-new) — sftp v3 has no
        // O_EXCL, so it is checked with stat (documented TOCTOU window).
        let final_remote = self.remote(p)?;
        self.check_final_absent(&final_remote).await?;
        let parent = self.remote_parent(p)?;
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        let staging = format!("{parent}/{PARTIAL_PREFIX}eph.{seq}");
        let file = self
            .session
            .open_with_flags(
                &staging,
                // EXCLUDE (atomic create-new): if the server pre-planted the
                // predictable staging path as a symlink outside base, the
                // open FAILS instead of following it and writing into the
                // target (write containment — ADR 0013 / threat model §14).
                OpenFlags::CREATE | OpenFlags::EXCLUDE | OpenFlags::WRITE | OpenFlags::TRUNCATE,
            )
            .await
            .map_err(|e| map_err(&e))?;
        Ok(Box::new(SftpSink {
            session: self.session(),
            file: Some(file),
            staging,
            final_remote,
        }))
    }

    async fn partial_digest(&self, p: &VPath, len: u64) -> Result<Option<[u8; 32]>, Error> {
        use sha2::{Digest, Sha256};
        use tokio::io::AsyncReadExt as _;
        // Same stable staging as open_resumable (#35): SHA-256 of its first
        // `len` bytes.
        let staging = self.stable_partial(p)?;
        // No staging = no digest (the engine degrades to Length). The server
        // can signal absence in several ways; any failure opening the
        // ephemeral staging is treated as "there is none".
        let Ok(mut file) = self
            .session
            .open_with_flags(&staging, OpenFlags::READ)
            .await
        else {
            return Ok(None);
        };
        let mut hasher = Sha256::new();
        let mut remaining = len;
        let mut buf = vec![0u8; 64 * 1024];
        while remaining > 0 {
            let want = usize::try_from(remaining.min(buf.len() as u64)).unwrap_or(buf.len());
            let n = file
                .read(&mut buf[..want])
                .await
                .map_err(|_| Error::Io { retryable: false })?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            remaining -= n as u64;
        }
        if remaining > 0 {
            // Staging shorter than `len`: without the full prefix → Length.
            return Ok(None);
        }
        Ok(Some(hasher.finalize().into()))
    }

    async fn open_resumable(&self, p: &VPath) -> Result<(Box<dyn ByteSink>, u64), Error> {
        let final_remote = self.remote(p)?;
        self.check_final_absent(&final_remote).await?;
        let staging = self.stable_partial(p)?;
        // A PRE-EXISTING staging must be a regular file: if the server
        // pre-planted it as a symlink (the name is deterministic), resuming
        // in APPEND would write into the target outside base. It is
        // rejected (EXCLUDE cannot be used: resume legitimately reopens a
        // partial). Documented TOCTOU, of the same class as
        // check_final_absent.
        match self.session.symlink_metadata(&staging).await {
            Ok(md) if md.file_type().is_symlink() => {
                return Err(Error::Conflict {
                    conflict: ConflictKind::TypeMismatch,
                });
            }
            _ => {}
        }
        // Opens (or creates) the staging in APPEND: if there were bytes from
        // a previous copy, it resumes after them (ADR 0012).
        let mut file = self
            .session
            .open_with_flags(
                &staging,
                OpenFlags::CREATE | OpenFlags::WRITE | OpenFlags::APPEND,
            )
            .await
            .map_err(|e| map_err(&e))?;
        let already = file.metadata().await.map_err(|e| map_err(&e))?.len();
        // The client's write offset starts at 0 even though the flag is
        // APPEND (sftp carries the explicit offset in every WRITE): it must
        // be positioned at the end to APPEND and not overwrite what was
        // already written.
        if already > 0 {
            file.seek(std::io::SeekFrom::Start(already))
                .await
                .map_err(|_| Error::Io { retryable: false })?;
        }
        Ok((
            Box::new(SftpSink {
                session: self.session(),
                file: Some(file),
                staging,
                final_remote,
            }),
            already,
        ))
    }

    async fn mkdir(&self, p: &VPath) -> Result<(), Error> {
        let remote = self.remote(p)?;
        // v3 returns a generic `Failure` if it already exists: checked
        // beforehand to give an honest `Conflict` (the engine distinguishes
        // it).
        if self.exists(&remote).await? {
            return Err(Error::Conflict {
                conflict: ConflictKind::Exists,
            });
        }
        self.session
            .create_dir(remote)
            .await
            .map_err(|e| map_err(&e))
    }

    /// #314: `SSH_FXP_SETSTAT` with ONLY the permissions field.
    ///
    /// The other attribute fields go to `None` on purpose: `setstat` fixes
    /// whatever it is sent, so filling size or dates with whatever had been
    /// read before would turn a `chmod` into a `touch` with a race inside.
    async fn set_mode(&self, p: &VPath, mode: u32) -> Result<(), Error> {
        let remote = self.remote(p)?;
        let attrs = russh_sftp::protocol::FileAttributes {
            permissions: Some(mode),
            ..Default::default()
        };
        self.session
            .set_metadata(remote, attrs)
            .await
            .map_err(|e| map_err(&e))
    }

    async fn remove(&self, p: &VPath) -> Result<(), Error> {
        let remote = self.remote(p)?;
        let md = self
            .session
            .symlink_metadata(&remote)
            .await
            .map_err(|e| map_err(&e))?;
        if md.is_dir() {
            self.session
                .remove_dir(remote)
                .await
                .map_err(|e| map_err(&e))
        } else {
            // remove_file deletes files AND symlinks (never follows the link).
            self.session
                .remove_file(remote)
                .await
                .map_err(|e| map_err(&e))
        }
    }

    /// GC of orphaned staging (#11, ADR 0012): sweeps `dir`'s
    /// `.norte-partial.*` whose mtime exceeds `older_than`, recognized by
    /// their exact SHAPE (`is_norte_partial`) — a real user file with the
    /// prefix is never touched (H2). Staging names are ASCII by
    /// construction: russh-sftp's lossy decoding (#37) cannot produce a
    /// false positive (U+FFFD does not match the shape).
    async fn gc_partials(
        &self,
        dir: &VPath,
        older_than: std::time::Duration,
    ) -> Result<usize, Error> {
        let parent = self.remote(dir)?;
        let dirents = self
            .session
            .read_dir(&parent)
            .await
            .map_err(|e| map_err(&e))?;
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let mut removed = 0usize;
        for dent in dirents {
            let name = dent.file_name();
            if !is_norte_partial(&name) {
                continue;
            }
            // Age by mtime (sftp v3: u32 seconds); left alone when mtime is
            // not readable (conservative, like the local provider).
            let old = dent
                .metadata()
                .mtime
                .is_some_and(|m| now_secs.saturating_sub(u64::from(m)) >= older_than.as_secs());
            if !old {
                continue;
            }
            let path = if parent.ends_with('/') {
                format!("{parent}{name}")
            } else {
                format!("{parent}/{name}")
            };
            // An individual failure counts as not-removed, without aborting.
            if self.session.remove_file(&path).await.is_ok() {
                removed += 1;
            }
        }
        Ok(removed)
    }

    async fn rename(&self, from: &VPath, to: &VPath) -> Result<(), Error> {
        let from_r = self.remote(from)?;
        let to_r = self.remote(to)?;
        // v3 rename does not guarantee no-replace: the destination is
        // checked beforehand (documented TOCTOU window) to give `Conflict`,
        // not overwrite.
        if self.exists(&to_r).await? {
            return Err(Error::Conflict {
                conflict: ConflictKind::Exists,
            });
        }
        self.session
            .rename(from_r, to_r)
            .await
            .map_err(|e| map_err(&e))
    }

    async fn trash(&self, p: &VPath, id: &trash::TrashId) -> Result<Option<VPath>, Error> {
        if !self.logical_trash {
            return Err(Error::Unsupported);
        }
        // DETERMINISTIC entry from the engine's id (#99): a retry points at
        // the same `.norte-trash/<id>/`. `plan` validates `p` (rejects
        // trashing the trash itself, ADR 0019) and gives the root.
        let paths = trash::plan(p, &id.as_segment())?;
        let trash_root = paths.dir.parent().ok_or(Error::Unsupported)?;

        // Idempotence: if the victim is no longer there, this op may have
        // applied in a previous, transient attempt. If our deterministic
        // payload exists, return it (recovers the `reversal_ref`); if not,
        // it is a genuine `NotFound` (the victim never existed), same as
        // `remove`.
        match self.stat(p).await {
            Ok(_) => {}
            Err(Error::NotFound) => {
                return match self.stat(&paths.payload).await {
                    // The payload is only claimed if the entry's
                    // `.norte-info` decodes to `p` (same victim): a
                    // FOREIGN entry with the same id is NOT ours (review
                    // rust MAJOR).
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

        // `.norte-trash/` is idempotent: under concurrency the losing
        // `create_dir` may give a generic `Failure` (→ `Io`) instead of
        // `Conflict`; if it already exists, it is benign.
        self.ensure_dir_idempotent(&trash_root).await?;

        // The `<id>/` entry: `Conflict::Exists` is OUR partial (info absent
        // or decodes to `p`) → continues; a FOREIGN info with the same id is
        // a REAL collision (the id is fixed, it is no longer regenerated) →
        // propagated without overwriting its metadata nor mixing the tree
        // (review rust MAJOR).
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

        // Writes `.norte-info` BEFORE moving: if the rename fails, the
        // source stays intact and there is only an orphaned info (cleanable
        // garbage), never a payload without metadata. `deleted_ms` comes
        // from the id (the same on every retry).
        let info = trash::info_encode(p, id.deleted_ms());
        let mut sink = self.write(&paths.info).await?;
        sink.write(Bytes::from(info)).await?;
        sink.commit().await?;

        // Moves the whole tree (a single rename by the server — ADR 0009,
        // entries_total = 1).
        self.rename(p, &paths.payload).await?;
        // LOGICAL trash: the payload IS the recoverable path → reversal_ref.
        Ok(Some(paths.payload))
    }

    /// The logical trash chooses its destination (`.norte-trash/<id>/payload`),
    /// so it always names it; without it there is no trash to promise.
    fn trash_restorable(&self) -> bool {
        self.logical_trash
    }

    async fn read_link(&self, p: &VPath) -> Result<Vec<u8>, Error> {
        let remote = self.remote(p)?;
        // A non-symlink gives an honest TypeMismatch (v3 returns a generic
        // Failure for readlink on a normal file).
        let md = self
            .session
            .symlink_metadata(&remote)
            .await
            .map_err(|e| map_err(&e))?;
        if !md.is_symlink() {
            return Err(Error::Conflict {
                conflict: ConflictKind::TypeMismatch,
            });
        }
        // RAW target bytes (rule 1): the target may be `../../` — it is
        // DATA, never resolved.
        let target = self
            .session
            .read_link(remote)
            .await
            .map_err(|e| map_err(&e))?;
        // russh-sftp decodes lossily: a non-UTF8 target arrives mangled as
        // U+FFFD. Reliable raw bytes cannot be returned → clean rejection
        // (Finding A / ADR 0013 D2), never a corrupt target.
        if target.contains('\u{FFFD}') {
            return Err(Error::InvalidPath);
        }
        Ok(target.into_bytes())
    }

    async fn symlink(&self, link: &VPath, target: &[u8], _kind: SymlinkKind) -> Result<(), Error> {
        let link_r = self.remote(link)?;
        // The target is raw bytes; sftp (russh-sftp) requires UTF-8.
        let target = std::str::from_utf8(target).map_err(|_| Error::InvalidPath)?;
        if self.exists(&link_r).await? {
            return Err(Error::Conflict {
                conflict: ConflictKind::Exists,
            });
        }
        self.session
            .symlink(link_r, target)
            .await
            .map_err(|e| map_err(&e))
    }

    async fn node_id(&self, p: &VPath, _follow: FollowLinks) -> Result<Option<NodeId>, Error> {
        // SFTP exposes no stable identity (FileAttributes carries no inode):
        // the engine degrades to a heuristic and `Follow` over dir-symlinks
        // answers `Unsupported` — the containment we want (ADR 0013). It is
        // stat-ed anyway to propagate an honest NotFound.
        let remote = self.remote(p)?;
        self.session
            .symlink_metadata(remote)
            .await
            .map_err(|e| map_err(&e))?;
        Ok(None)
    }
}

impl SftpProvider {
    /// The FINAL destination must not exist (contract of `write`/
    /// `open_resumable`; the overwrite policy belongs to the core, not the
    /// provider).
    async fn check_final_absent(&self, remote: &str) -> Result<(), Error> {
        if self.exists(remote).await? {
            return Err(Error::Conflict {
                conflict: ConflictKind::Exists,
            });
        }
        Ok(())
    }

    /// Does `remote` exist? (lstat; a symlink counts as existing.)
    async fn exists(&self, remote: &str) -> Result<bool, Error> {
        match self.session.symlink_metadata(remote).await {
            Ok(_) => Ok(true),
            Err(e) => match map_err(&e) {
                Error::NotFound => Ok(false),
                other => Err(other),
            },
        }
    }
}

/// Write sink over sftp: bytes go to a remote staging file; `commit` renames
/// to the final path (contract of [`ByteSink`], ADR 0012).
struct SftpSink {
    session: Arc<SftpSession>,
    file: Option<russh_sftp::client::fs::File>,
    staging: String,
    final_remote: String,
}

impl SftpSink {
    async fn remove_staging(&self) {
        let _ = self.session.remove_file(&self.staging).await;
    }
}

#[async_trait]
impl ByteSink for SftpSink {
    async fn write(&mut self, chunk: Bytes) -> Result<(), Error> {
        let file = self.file.as_mut().ok_or(Error::Io { retryable: false })?;
        file.write_all(&chunk)
            .await
            .map_err(|_| Error::Io { retryable: false })
    }

    async fn commit(mut self: Box<Self>) -> Result<(), Error> {
        if let Some(mut file) = self.file.take() {
            file.flush()
                .await
                .map_err(|_| Error::Io { retryable: false })?;
            file.sync_all().await.map_err(|e| map_err(&e))?;
            drop(file);
        }
        // The final destination must not exist (create-new): checked in
        // write()/open_resumable; the window up to here is TOCTOU (v3 has no
        // atomic rename) — if something appears, Conflict and the staging
        // stays behind for the GC.
        match self.session.symlink_metadata(&self.final_remote).await {
            Ok(_) => {
                return Err(Error::Conflict {
                    conflict: ConflictKind::Exists,
                });
            }
            Err(e) if matches!(map_err(&e), Error::NotFound) => {}
            Err(e) => {
                return Err(map_err(&e));
            }
        }
        self.session
            .rename(&self.staging, &self.final_remote)
            .await
            .map_err(|e| map_err(&e))?;
        Ok(())
    }

    async fn abort(mut self: Box<Self>) -> Result<(), Error> {
        self.file.take();
        self.remove_staging().await;
        Ok(())
    }

    async fn keep(mut self: Box<Self>) -> Result<(), Error> {
        // Keeps the staging for a later open_resumable (ADR 0012): durably
        // flushes and releases without renaming or deleting.
        if let Some(file) = self.file.take() {
            let _ = file.sync_all().await;
        }
        Ok(())
    }
}

/// Producer of the read stream, isolated so as not to drag generics into the
/// trait method.
mod read_stream {
    use super::{ByteStream, Bytes, Error, READ_CHUNK, StreamExt};
    use tokio::io::AsyncReadExt;

    /// Stream of chunks from an open sftp `File` (already positioned at the
    /// offset). `len` bounds the bytes to deliver (`None` = until EOF).
    pub(super) fn sftp_read_stream(
        file: russh_sftp::client::fs::File,
        len: Option<u64>,
    ) -> ByteStream {
        let s = futures::stream::unfold(
            (file, len, false),
            |(mut file, mut remaining, done)| async move {
                if done {
                    return None;
                }
                let want = match remaining {
                    Some(0) => return None,
                    Some(n) => usize::try_from(n.min(READ_CHUNK as u64)).unwrap_or(READ_CHUNK),
                    None => READ_CHUNK,
                };
                let mut buf = vec![0u8; want];
                match file.read(&mut buf).await {
                    Ok(0) => None,
                    Ok(n) => {
                        buf.truncate(n);
                        if let Some(rem) = &mut remaining {
                            *rem -= n as u64;
                        }
                        Some((Ok(Bytes::from(buf)), (file, remaining, false)))
                    }
                    Err(_) => Some((Err(Error::Io { retryable: false }), (file, remaining, true))),
                }
            },
        );
        s.boxed()
    }
}
