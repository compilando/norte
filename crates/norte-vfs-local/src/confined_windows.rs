//! Writing under a root without being able to escape it, on Windows (#217,
//! ADR 0160). The unix side is `confined`; the contract is the same and so
//! are the tests that pin it, with one verdict on the safe side: a link in
//! a component is NEVER crossed here, not even a relative one that stays
//! inside (unix follows that one).
//!
//! Nothing below the root reaches Win32 as a path. The root's handle is
//! held, each component is opened relative to its parent's handle
//! (`win_nt::open_child`), a file is published by renaming its own handle
//! into its directory's handle without replacing, and a staging is deleted
//! through its own handle — so between opening and publishing there is no
//! name left for anyone to redirect.

use std::fs::File;
use std::os::windows::fs::{MetadataExt as _, OpenOptionsExt as _};
use std::path::Path;
use std::sync::Arc;

use norte_proto::{ConflictKind, Entry, Error, Segment, VPath};
use windows_sys::Wdk::Storage::FileSystem::{
    FILE_CREATE, FILE_DIRECTORY_FILE, FILE_NON_DIRECTORY_FILE, FILE_OPEN, FILE_OPEN_IF,
    FILE_OPEN_REPARSE_POINT,
};
use windows_sys::Win32::Storage::FileSystem::{
    DELETE, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_GENERIC_READ,
    FILE_GENERIC_WRITE, FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES, FILE_SHARE_READ, FILE_TRAVERSE,
    READ_CONTROL, SYNCHRONIZE,
};

use crate::provider::map_io;
use crate::win_nt::{self, ChildError};

/// What a directory handle needs: to be traversed, listed and looked at.
const DIR_ACCESS: u32 = FILE_LIST_DIRECTORY | FILE_TRAVERSE | FILE_READ_ATTRIBUTES | SYNCHRONIZE;

/// An opened root, and the only place anything under it is addressed from.
#[derive(Debug)]
pub(crate) struct WinRoot {
    dir: File,
}

impl WinRoot {
    /// Opens `dir` as a confined root. The root itself is opened by path
    /// and FOLLOWING links: which directory it is was the caller's choice.
    ///
    /// BLOCKING: goes inside `spawn_blocking` (hard rule 2).
    pub(crate) fn open(dir: &Path) -> Result<Self, Error> {
        let dir = std::fs::OpenOptions::new()
            .access_mode(DIR_ACCESS)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(dir)
            .map_err(|e| map_io(&e))?;
        if !dir.metadata().map_err(|e| map_io(&e))?.is_dir() {
            return Err(Error::Conflict {
                conflict: ConflictKind::TypeMismatch,
            });
        }
        Ok(Self { dir })
    }

    /// The `parents` directory under the root, one component at a time.
    fn resolve_dir(&self, parents: &[Segment]) -> Result<File, Error> {
        // A fresh file object for the root too: the caller may hold it
        // across a publish while another operation walks.
        let mut current = win_nt::nt_create(
            &self.dir,
            &[],
            DIR_ACCESS,
            FILE_OPEN,
            FILE_DIRECTORY_FILE,
            win_nt::SHARE_ALL,
        )
        .map_err(|e| map_io(&e))?;
        for seg in parents {
            current = win_nt::open_child(&current, &wide(seg)?, DIR_ACCESS, FILE_DIRECTORY_FILE)
                .map_err(child_error)?;
        }
        Ok(current)
    }

    /// The parent of `rel`, and the last segment as UTF-16.
    fn parent_of(&self, rel: &[Segment]) -> Result<(File, Vec<u16>), Error> {
        let (last, parents) = rel.split_last().ok_or(Error::InvalidPath)?;
        Ok((self.resolve_dir(parents)?, wide(last)?))
    }

    /// Opens `rel`'s leaf itself — a link as the link — for `access`.
    fn open_leaf(&self, rel: &[Segment], access: u32, options: u32) -> Result<File, Error> {
        let (dir, name) = self.parent_of(rel)?;
        win_nt::nt_create(
            &dir,
            &name,
            access | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
            FILE_OPEN,
            options | FILE_OPEN_REPARSE_POINT,
            win_nt::SHARE_ALL,
        )
        .map_err(|e| map_io(&e))
    }

    /// `None` where the volume gives no identity (index 0, some SMB
    /// servers): "cannot tell", which the core already degrades on, rather
    /// than an error that would fail the copy.
    pub(crate) fn root_id(&self) -> Result<Option<norte_vfs::NodeId>, Error> {
        identity(&self.dir)
    }

    pub(crate) fn mkdir(&self, rel: &[Segment]) -> Result<(), Error> {
        let (dir, name) = self.parent_of(rel)?;
        win_nt::nt_create(
            &dir,
            &name,
            FILE_LIST_DIRECTORY | SYNCHRONIZE,
            FILE_CREATE,
            FILE_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT,
            win_nt::SHARE_ALL,
        )
        .map(drop)
        .map_err(|e| exists_or(&e))
    }

    /// `lstat`: describes the LINK, never its target.
    pub(crate) fn stat(&self, rel: &[Segment], path: VPath) -> Result<Entry, Error> {
        let md = self
            .open_leaf(rel, 0, 0)?
            .metadata()
            .map_err(|e| map_io(&e))?;
        Ok(crate::provider::entry_from(
            path,
            &md,
            &norte_vfs::AttrRequest::default(),
        ))
    }

    pub(crate) fn node_id(&self, rel: &[Segment]) -> Result<Option<norte_vfs::NodeId>, Error> {
        identity(&self.open_leaf(rel, 0, 0)?)
    }

    /// Deletes a LEAF about to be replaced: a file, or a link as the link.
    /// A directory in its place is `TypeMismatch`, touching nothing.
    pub(crate) fn remove(&self, rel: &[Segment]) -> Result<(), Error> {
        let leaf = self.open_leaf(rel, DELETE, 0)?;
        let md = leaf.metadata().map_err(|e| map_io(&e))?;
        if md.is_dir() && !md.file_type().is_symlink() {
            return Err(Error::Conflict {
                conflict: ConflictKind::TypeMismatch,
            });
        }
        win_nt::delete_by_handle(&leaf).map_err(|e| map_io(&e))
    }

    /// Deletes the EMPTY directory in `rel`; a full one is refused by the
    /// kernel and comes back as `TypeMismatch`, as on unix.
    pub(crate) fn rmdir(&self, rel: &[Segment]) -> Result<(), Error> {
        let leaf = self.open_leaf(rel, DELETE, FILE_DIRECTORY_FILE)?;
        if leaf
            .metadata()
            .map_err(|e| map_io(&e))?
            .file_type()
            .is_symlink()
        {
            return Err(Error::Conflict {
                conflict: ConflictKind::TypeMismatch,
            });
        }
        win_nt::delete_by_handle(&leaf).map_err(|e| map_io(&e))
    }

    /// A sink for `rel`: an ephemeral staging created in the resolved
    /// directory and published by handle into that same directory.
    pub(crate) fn open_write(&self, rel: &[Segment]) -> Result<WinStaging, Error> {
        let (dir, final_name) = self.parent_of(rel)?;
        ensure_free(&dir, &final_name)?;
        let last = rel.last().ok_or(Error::InvalidPath)?;
        let staging = crate::provider::ephemeral_partial_name(last.as_bytes());
        let file = win_nt::nt_create(
            &dir,
            &ascii_wide(&staging),
            FILE_GENERIC_WRITE | DELETE | SYNCHRONIZE,
            FILE_CREATE,
            FILE_NON_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT,
            FILE_SHARE_READ,
        )
        .map_err(|e| exists_or(&e))?;
        Ok(WinStaging {
            dir,
            file,
            final_name,
            stable: false,
        })
    }

    /// Like [`Self::open_write`], with the STABLE staging a later resume
    /// finds again (#297), continued after the bytes it already has.
    ///
    /// The name is predictable, so what is there may have been planted
    /// (#298): a reparse point, a file with another hard link or one owned
    /// by someone else is refused (`EscapesRoot`). An owner of its own
    /// could read our bytes as they arrive and rewrite what gets published.
    pub(crate) fn open_resumable(&self, rel: &[Segment]) -> Result<(WinStaging, u64), Error> {
        use std::io::{Seek as _, SeekFrom};
        let (dir, final_name) = self.parent_of(rel)?;
        ensure_free(&dir, &final_name)?;
        let last = rel.last().ok_or(Error::InvalidPath)?;
        let staging = ascii_wide(&crate::provider::stable_partial_name(last.as_bytes()));
        // Looked at first, without constraining its type: a directory or a
        // junction planted with this name would make the open below fail
        // with a bare "access denied", and the reader deserves to know it
        // was something else sitting there. `check_ours` still checks what
        // was actually opened.
        match win_nt::nt_create(
            &dir,
            &staging,
            FILE_READ_ATTRIBUTES | READ_CONTROL | SYNCHRONIZE,
            FILE_OPEN,
            FILE_OPEN_REPARSE_POINT,
            win_nt::SHARE_ALL,
        ) {
            Ok(existing) => check_ours(&existing)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(map_io(&e)),
        }
        let mut file = win_nt::nt_create(
            &dir,
            &staging,
            FILE_GENERIC_WRITE | FILE_GENERIC_READ | DELETE | READ_CONTROL | SYNCHRONIZE,
            FILE_OPEN_IF,
            FILE_NON_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT,
            FILE_SHARE_READ,
        )
        .map_err(|e| map_io(&e))?;
        check_ours(&file)?;
        let already = file.seek(SeekFrom::End(0)).map_err(|e| map_io(&e))?;
        Ok((
            WinStaging {
                dir,
                file,
                final_name,
                stable: true,
            },
            already,
        ))
    }

    /// The digest of the first `len` bytes of `rel`'s stable partial, with
    /// the same checks as [`Self::open_resumable`]; `None` if there is no
    /// partial of ours that long.
    pub(crate) fn partial_digest(
        &self,
        rel: &[Segment],
        len: u64,
    ) -> Result<Option<[u8; 32]>, Error> {
        use sha2::{Digest as _, Sha256};
        use std::io::Read as _;
        let (dir, _) = self.parent_of(rel)?;
        let last = rel.last().ok_or(Error::InvalidPath)?;
        let staging = crate::provider::stable_partial_name(last.as_bytes());
        let file = match win_nt::nt_create(
            &dir,
            &ascii_wide(&staging),
            FILE_GENERIC_READ | READ_CONTROL | SYNCHRONIZE,
            FILE_OPEN,
            FILE_NON_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT,
            win_nt::SHARE_ALL,
        ) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(map_io(&e)),
        };
        if check_ours(&file).is_err() {
            return Ok(None);
        }
        let mut reader = file.take(len);
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 64 * 1024];
        let mut seen: u64 = 0;
        loop {
            let n = reader.read(&mut buf).map_err(|e| map_io(&e))?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            seen += n as u64;
        }
        Ok((seen >= len).then(|| hasher.finalize().into()))
    }
}

/// A staging under a confined root, with what publishing it needs: the
/// directory's handle and the final name. Its own name is not kept: after
/// creation it is only ever reached through `file`.
#[derive(Debug)]
pub(crate) struct WinStaging {
    dir: File,
    file: File,
    final_name: Vec<u16>,
    stable: bool,
}

/// `name` must not exist yet in `dir`: `Provider::write` promises a NEW
/// file and says so at open time. The guarantee is the no-replace publish;
/// this only spares transferring a whole file for a conflict.
fn ensure_free(dir: &File, name: &[u16]) -> Result<(), Error> {
    match win_nt::nt_create(
        dir,
        name,
        FILE_READ_ATTRIBUTES | SYNCHRONIZE,
        FILE_OPEN,
        FILE_OPEN_REPARSE_POINT,
        win_nt::SHARE_ALL,
    ) {
        Ok(_) => Err(Error::Conflict {
            conflict: ConflictKind::Exists,
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(map_io(&e)),
    }
}

/// A resumed staging must be a plain file with one name, owned by us: a
/// reparse point or a second hard link would carry our bytes somewhere
/// else, and another owner keeps control of what we publish. The handle
/// needs `READ_CONTROL`.
fn check_ours(file: &File) -> Result<(), Error> {
    let md = file.metadata().map_err(|e| map_io(&e))?;
    if md.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(Error::Conflict {
            conflict: ConflictKind::EscapesRoot,
        });
    }
    if !md.is_file() {
        return Err(Error::Conflict {
            conflict: ConflictKind::TypeMismatch,
        });
    }
    if win_nt::link_count(file).map_err(|e| map_io(&e))? != 1
        || !win_nt::owned_by_us(file).map_err(|e| map_io(&e))?
    {
        return Err(Error::Conflict {
            conflict: ConflictKind::EscapesRoot,
        });
    }
    Ok(())
}

/// The node's identity, or `None` where the volume gives none (index 0).
fn identity(file: &File) -> Result<Option<norte_vfs::NodeId>, Error> {
    let id = win_nt::file_id(file).map_err(|e| map_io(&e))?;
    Ok((id.index != 0).then_some(id))
}

fn wide(seg: &Segment) -> Result<Vec<u16>, Error> {
    win_nt::wide_component(seg.as_bytes()).ok_or(Error::InvalidPath)
}

/// A staging name: ASCII by construction (prefix, hex, digits, dots).
fn ascii_wide(name: &[u8]) -> Vec<u16> {
    debug_assert!(name.is_ascii(), "a staging name is ASCII");
    name.iter().map(|&b| u16::from(b)).collect()
}

fn child_error(e: ChildError) -> Error {
    match e {
        ChildError::Escapes => Error::Conflict {
            conflict: ConflictKind::EscapesRoot,
        },
        ChildError::Io(e) => map_io(&e),
    }
}

fn exists_or(e: &std::io::Error) -> Error {
    if e.kind() == std::io::ErrorKind::AlreadyExists {
        Error::Conflict {
            conflict: ConflictKind::Exists,
        }
    } else {
        map_io(e)
    }
}

// ---------- the handle the core sees ----------

/// [`norte_vfs::ConfinedRoot`] over a [`WinRoot`].
#[derive(Debug)]
pub(crate) struct WinConfinedRoot {
    root: Arc<WinRoot>,
    /// The root as a `VPath`, ONLY to name what `stat` returns.
    vpath: VPath,
}

impl WinConfinedRoot {
    pub(crate) fn new(root: WinRoot, vpath: VPath) -> Self {
        Self {
            root: Arc::new(root),
            vpath,
        }
    }

    fn vpath_of(&self, rel: &[Segment]) -> VPath {
        rel.iter()
            .fold(self.vpath.clone(), |p, s| p.join(s.clone()))
    }
}

#[async_trait::async_trait]
impl norte_vfs::ConfinedRoot for WinConfinedRoot {
    async fn root_id(&self) -> Result<Option<norte_vfs::NodeId>, Error> {
        let root = Arc::clone(&self.root);
        crate::provider::blocking(move || root.root_id()).await
    }

    async fn mkdir(&self, rel: &[Segment]) -> Result<(), Error> {
        let (root, rel) = (Arc::clone(&self.root), rel.to_vec());
        crate::provider::blocking(move || root.mkdir(&rel)).await
    }

    async fn write(&self, rel: &[Segment]) -> Result<Box<dyn norte_vfs::ByteSink>, Error> {
        let (root, rel) = (Arc::clone(&self.root), rel.to_vec());
        let staging = crate::provider::blocking(move || root.open_write(&rel)).await?;
        Ok(Box::new(WinSink::new(staging, 0)))
    }

    /// Windows creates no links without a privilege the provider does not
    /// assume (#220): the same answer as `Provider::symlink` there.
    async fn symlink(
        &self,
        _rel: &[Segment],
        _target: &[u8],
        _kind: norte_vfs::SymlinkKind,
    ) -> Result<(), Error> {
        Err(Error::Unsupported)
    }

    fn resumes(&self) -> bool {
        true
    }

    async fn open_resumable(
        &self,
        rel: &[Segment],
    ) -> Result<(Box<dyn norte_vfs::ByteSink>, u64), Error> {
        let (root, rel) = (Arc::clone(&self.root), rel.to_vec());
        let (staging, already) =
            crate::provider::blocking(move || root.open_resumable(&rel)).await?;
        Ok((Box::new(WinSink::new(staging, already)), already))
    }

    async fn stat(&self, rel: &[Segment]) -> Result<Entry, Error> {
        let path = self.vpath_of(rel);
        let (root, rel) = (Arc::clone(&self.root), rel.to_vec());
        crate::provider::blocking(move || root.stat(&rel, path)).await
    }

    async fn node_id(&self, rel: &[Segment]) -> Result<Option<norte_vfs::NodeId>, Error> {
        let (root, rel) = (Arc::clone(&self.root), rel.to_vec());
        crate::provider::blocking(move || root.node_id(&rel)).await
    }

    async fn remove(&self, rel: &[Segment]) -> Result<(), Error> {
        let (root, rel) = (Arc::clone(&self.root), rel.to_vec());
        crate::provider::blocking(move || root.remove(&rel)).await
    }

    async fn rmdir(&self, rel: &[Segment]) -> Result<(), Error> {
        let (root, rel) = (Arc::clone(&self.root), rel.to_vec());
        crate::provider::blocking(move || root.rmdir(&rel)).await
    }

    async fn partial_digest(&self, rel: &[Segment], len: u64) -> Result<Option<[u8; 32]>, Error> {
        let (root, rel) = (Arc::clone(&self.root), rel.to_vec());
        crate::provider::blocking(move || root.partial_digest(&rel, len)).await
    }
}

/// The sink of a confined write. It holds the directory's handle and the
/// staging's own handle: publishing and discarding name nothing.
#[derive(Debug)]
struct WinSink {
    staging: Option<WinStaging>,
    /// Bytes delivered, holes included: [`crate::provider::write_maybe_sparse`]'s
    /// anchor, starting at what a resumed staging already had.
    pos: u64,
    stable: bool,
}

impl WinSink {
    fn new(staging: WinStaging, already: u64) -> Self {
        let stable = staging.stable;
        Self {
            staging: Some(staging),
            pos: already,
            stable,
        }
    }
}

/// Deletes the staging through its handle, then lets go of it.
fn discard(staging: WinStaging) -> Result<(), Error> {
    let out = win_nt::delete_by_handle(&staging.file).map_err(|e| map_io(&e));
    drop(staging);
    out
}

#[async_trait::async_trait]
impl norte_vfs::ByteSink for WinSink {
    async fn write(&mut self, chunk: bytes::Bytes) -> Result<(), Error> {
        let mut staging = self.staging.take().ok_or(Error::Io { retryable: false })?;
        let mut pos = self.pos;
        let (staging, pos, res) = tokio::task::spawn_blocking(move || {
            let res = crate::provider::write_maybe_sparse(&mut staging.file, &mut pos, &chunk)
                .map_err(|e| map_io(&e));
            (staging, pos, res)
        })
        .await
        .map_err(|_| Error::Internal { panic: true })?;
        self.staging = Some(staging);
        self.pos = pos;
        res
    }

    async fn commit(mut self: Box<Self>) -> Result<(), Error> {
        let staging = self.staging.take().ok_or(Error::Io { retryable: false })?;
        crate::provider::blocking(move || {
            staging.file.sync_all().map_err(|e| map_io(&e))?;
            match win_nt::rename_beneath(&staging.file, &staging.dir, &staging.final_name, false) {
                Ok(()) => Ok(()),
                // A publish that does not publish leaves no staging behind.
                Err(e) => {
                    let _ = discard(staging);
                    Err(exists_or(&e))
                }
            }
        })
        .await
    }

    async fn abort(mut self: Box<Self>) -> Result<(), Error> {
        let staging = self.staging.take().ok_or(Error::Io { retryable: false })?;
        crate::provider::blocking(move || discard(staging)).await
    }

    /// An ephemeral staging cannot be found again, so keeping it would only
    /// litter: `keep` deletes it. A stable one is synced and left in place.
    async fn keep(mut self: Box<Self>) -> Result<(), Error> {
        let staging = self.staging.take().ok_or(Error::Io { retryable: false })?;
        if !self.stable {
            return crate::provider::blocking(move || discard(staging)).await;
        }
        crate::provider::blocking(move || staging.file.sync_all().map_err(|e| map_io(&e))).await
    }
}

impl Drop for WinSink {
    fn drop(&mut self) {
        // Dropped without commit or abort: an ephemeral staging goes, a
        // stable one stays for the resume that will look for it.
        if let Some(staging) = self.staging.take()
            && !self.stable
        {
            let _ = discard(staging);
        }
    }
}
