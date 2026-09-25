//! [`ConfinedRoot`]: reading UNDER a directory without being able to escape
//! it, with a budget.
//!
//! Exists for the plugin-host's `location` capability (ADR 0057): an
//! approved guest gets an opaque token, not a path, and reads what's under
//! the directory the pane is listing. None of this can live outside this
//! crate — hard rule 2 says only here is `std::fs` touched, and confinement
//! is the kernel's ([`LocalRoot`], `openat2(RESOLVE_BENEATH)`, the same
//! mechanism that closed #164), not a string check.
//!
//! # What it guarantees, exactly
//!
//! - **No escaping.** An INTERIOR `..` is legitimate (`sub/../f`); one that
//!   climbs above the root is [`LocationError::Escapes`], and so is an
//!   absolute path — to `RESOLVE_BENEATH` an absolute path already starts
//!   outside.
//! - **The last component isn't followed.** It's opened with `O_NOFOLLOW`:
//!   a final symlink is a `Symlink` that can be `stat`-ed, never a file
//!   read without knowing what it points at. INTERMEDIATE components are
//!   governed by [`LocalRoot`] with #164's criterion (followed if they
//!   don't escape).
//! - **Paid for per call, and also when it fails.** If an error didn't
//!   spend budget, probing the tree by failing on purpose would be free.
//! - **A protected root falling INSIDE isn't entered** (#238). Confining
//!   bounds from above and says nothing about what's below: with the root
//!   at `$XDG_CONFIG_HOME` — a directory a human lists without a second
//!   thought — the guest read `norte/secrets.age`, `norte/journal.db` and
//!   `norte/connections.toml`, and with the root at `/` it read the whole
//!   disk. The veto is by `(dev, ino)` of each directory along the path,
//!   not by comparing strings: a symlink pointing at the protected root
//!   gives the same inode.
//!
//! There's no `write` and there won't be: the capability is READ-ONLY.

use std::ffi::CString;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::Path;
use std::sync::Mutex;

use norte_proto::Segment;

use crate::confined::LocalRoot;

/// Ceilings of a location session. All fail-closed.
#[derive(Debug, Clone, Copy)]
pub struct Bounds {
    /// Byte ceiling of ONE read. Exceeding it is [`LocationError::TooLarge`],
    /// never a file silently truncated.
    pub max_read_bytes: u64,
    /// Ceiling on the whole session's calls, failures included.
    pub max_calls: u32,
    /// Ceiling on ACCUMULATED bytes the session ends up delivering.
    pub max_total_bytes: u64,
    /// Ceiling on entries a `list` returns.
    pub max_list_entries: u32,
}

impl Default for Bounds {
    /// What's enough for a git-status reader and little more: a large
    /// repository's index is a few MB.
    fn default() -> Self {
        Self {
            max_read_bytes: 16 * 1024 * 1024,
            max_calls: 4_096,
            max_total_bytes: 64 * 1024 * 1024,
            max_list_entries: 4_096,
        }
    }
}

/// What an entry is. Deliberately coarse: it's plenty for the guest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocationKind {
    /// Regular file.
    File,
    /// Directory.
    Dir,
    /// Symbolic link (NOT followed).
    Symlink,
    /// Anything else (fifo, socket, device).
    Other,
}

/// What `stat` returns: exactly the fields git's index stores, because
/// comparing only `mtime` is how a change made within the same second gets
/// lost.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocationMeta {
    /// What kind of node it is.
    pub kind: LocationKind,
    /// Size in bytes.
    pub size: u64,
    /// mtime, seconds.
    pub mtime_sec: i64,
    /// mtime, nanoseconds.
    pub mtime_nsec: u32,
    /// ctime, seconds.
    pub ctime_sec: i64,
    /// ctime, nanoseconds.
    pub ctime_nsec: u32,
    /// Inode number.
    pub ino: u64,
    /// Device.
    pub dev: u64,
    /// Mode (permissions + type), exactly as the system gives it.
    pub mode: u32,
}

/// An entry from a `list`: name in RAW BYTES (hard rule 1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocationDirent {
    /// The name, undecoded.
    pub name: Vec<u8>,
    /// What it is, if `readdir` said so; `Other` when it didn't.
    pub kind: LocationKind,
}

/// Why a confined read couldn't be served.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum LocationError {
    /// The relative path escapes the root (or is absolute, which to
    /// `RESOLVE_BENEATH` is the same thing).
    #[error("path escapes the confined root")]
    Escapes,
    /// Doesn't exist.
    #[error("not found")]
    NotFound,
    /// Denied: either the OS denied it, or the path enters a protected root
    /// (#238).
    ///
    /// **Both cases give the SAME error**, on purpose: the guest isn't told
    /// which directories exist and are untouchable. A distinct error would
    /// be an oracle for where the journal lives.
    #[error("permission denied")]
    Denied,
    /// The file exceeds `max_read_bytes`. It isn't truncated: it's reported.
    #[error("entry is larger than the read bound")]
    TooLarge,
    /// The session ran out of call or byte budget.
    #[error("location budget exhausted")]
    Budget,
    /// The node type doesn't support this operation (reading a directory,
    /// listing a file).
    #[error("wrong node type for this operation")]
    TypeMismatch,
    /// Any other I/O failure.
    #[error("i/o error")]
    Io,
}

/// What a session has spent.
#[derive(Debug, Default)]
struct Spent {
    calls: u32,
    bytes: u64,
}

/// Bounded reading UNDER a directory.
#[derive(Debug)]
pub struct ConfinedRoot {
    root: LocalRoot,
    bounds: Bounds,
    spent: Mutex<Spent>,
    /// `(dev, ino)` of the protected roots that fall inside this root
    /// (#238). Resolved ONCE, at open time: they're directories of this
    /// process and don't move under our feet for the duration of a call.
    forbidden: Vec<(u64, u64)>,
}

impl ConfinedRoot {
    /// Opens `dir` as a confined root.
    ///
    /// BLOCKING: goes inside `spawn_blocking` (hard rule 2).
    ///
    /// # Errors
    ///
    /// [`LocationError::NotFound`] or [`LocationError::Denied`] if the
    /// directory can't be opened, and `Denied` too if `dir` **is** one of
    /// the protected roots.
    ///
    /// `protected` are directories that don't get opened even if they fall
    /// inside the root (#238): confining bounds from above and says
    /// absolutely nothing about what's below, so without this a perfectly
    /// innocent root — the user's config directory, listed without a
    /// second thought — used to contain the journal, the secrets and the
    /// connections file. A protected path that doesn't exist or isn't
    /// inside costs nothing: it's ignored.
    ///
    /// ```
    /// # use norte_vfs_local::{Bounds, ConfinedRoot};
    /// let dir = tempfile::tempdir().unwrap();
    /// std::fs::write(dir.path().join("f"), b"hi").unwrap();
    /// std::fs::create_dir(dir.path().join("private")).unwrap();
    /// std::fs::write(dir.path().join("private/x"), b"secret").unwrap();
    /// let blocked = dir.path().join("private");
    /// let root = ConfinedRoot::open(dir.path(), Bounds::default(), &[blocked]).unwrap();
    /// assert_eq!(root.read(b"f").unwrap(), b"hi");
    /// assert!(root.read(b"private/x").is_err(), "the protected root is not crossed");
    /// ```
    pub fn open(
        dir: &Path,
        bounds: Bounds,
        protected: &[std::path::PathBuf],
    ) -> Result<Self, LocationError> {
        Self::open_verified(dir, bounds, protected, None)
    }

    /// Like [`Self::open`], requiring that what's opened be the node the
    /// caller ALREADY looked at (#241).
    ///
    /// `expect` is the `(dev, ino)` the caller observed when it decided
    /// this path was the root. Between that look and this `open` there's a
    /// window: the path is resolved again from `/`, following links and
    /// unconfined, so renaming a component in between would change the
    /// root to whatever whoever renamed it wanted. With the expected node,
    /// a root that has changed under our feet is refused instead of
    /// served.
    ///
    /// `None` is "I didn't look before", which is what [`Self::open`] does.
    ///
    /// # Errors
    ///
    /// Whatever [`Self::open`] returns, and [`LocationError::Denied`] if
    /// the opened node isn't the expected one.
    pub fn open_verified(
        dir: &Path,
        bounds: Bounds,
        protected: &[std::path::PathBuf],
        expect: Option<(u64, u64)>,
    ) -> Result<Self, LocationError> {
        let root = LocalRoot::open(dir).map_err(|e| from_proto(&e))?;
        if let Some(expected) = expect {
            let opened = crate::confined::node_id_of(root.raw_fd()).map_err(|e| from_proto(&e))?;
            if opened != expected {
                return Err(LocationError::Denied);
            }
        }
        // Resolved by `(dev, ino)` and not by path prefix: comparing
        // strings is what a symlink goes around, and the root opened here
        // may have arrived through one.
        let forbidden: Vec<(u64, u64)> = protected
            .iter()
            .filter_map(|p| {
                let fd = LocalRoot::open(p).ok()?;
                crate::confined::node_id_of(fd.raw_fd()).ok()
            })
            .collect();
        let own_id = crate::confined::node_id_of(root.raw_fd()).map_err(|e| from_proto(&e))?;
        if forbidden.contains(&own_id) {
            // The root ITSELF is protected. The caller already checks this
            // by path, but that check is on strings and this one is on
            // inodes.
            return Err(LocationError::Denied);
        }
        Ok(Self {
            root,
            bounds,
            spent: Mutex::new(Spent::default()),
            forbidden,
        })
    }

    /// Rejects a path that CROSSES or ENDS at a protected root (#238).
    ///
    /// Checks every prefix, not just the destination: without that,
    /// `norte/sub/x` would pass right over a veto on `norte`. It's a
    /// per-prefix resolution, i.e. O(n²) in syscalls over the path's depth
    /// — which here is two or three components, and the alternative
    /// (walking component by component on our own) is reimplementing what
    /// `openat2(RESOLVE_BENEATH)` already does well.
    ///
    /// With no protected roots inside, it doesn't cost a single syscall.
    fn ensure_allowed(&self, comps: &[Segment]) -> Result<(), LocationError> {
        if self.forbidden.is_empty() {
            return Ok(());
        }
        for up_to in 1..=comps.len() {
            let Ok(fd) = self.root.resolve_dir(&comps[..up_to]) else {
                // Doesn't resolve as a directory: either it doesn't exist,
                // or it's a file. In neither case is it a protected root
                // one could pass through, and the real error will come
                // from the caller.
                break;
            };
            let id = crate::confined::node_id_of(fd.as_raw_fd()).map_err(|e| from_proto(&e))?;
            if self.forbidden.contains(&id) {
                return Err(LocationError::Denied);
            }
        }
        Ok(())
    }

    /// Reads a file under the root, whole.
    ///
    /// # Errors
    ///
    /// [`LocationError`]'s: `Escapes` if the path escapes, `TooLarge` if it
    /// exceeds `max_read_bytes`, `Budget` if the session ran out.
    pub fn read(&self, rel: &[u8]) -> Result<Vec<u8>, LocationError> {
        self.charge_call()?;
        let (parent, name) = Self::split(rel)?;
        self.ensure_allowed(&Self::components(rel)?)?;
        let dir = self.dir_fd(&parent)?;
        let file = openat_read(dir.as_raw_fd(), name.as_ref())?;
        let meta = file.metadata().map_err(|e| from_io(&e))?;
        if !meta.is_file() {
            return Err(LocationError::TypeMismatch);
        }
        if meta.len() > self.bounds.max_read_bytes {
            return Err(LocationError::TooLarge);
        }
        self.charge_bytes(meta.len())?;
        // `take` and not a bare `read_to_end` (#240): the cap and the
        // charge both came from `st_size`, and `st_size` can lie — a file
        // another process is appending to, or anything on a FUSE the user
        // controls. Without the `take`, that file would land whole in the
        // host's memory and the session's budget would undercount.
        let cap = self.bounds.max_read_bytes;
        let mut buf = Vec::with_capacity(usize::try_from(meta.len()).unwrap_or(0));
        let read_len = std::io::Read::read_to_end(&mut std::io::Read::take(file, cap), &mut buf)
            .map_err(|e| from_io(&e))?;
        let read_len = u64::try_from(read_len).unwrap_or(u64::MAX);
        if read_len > cap {
            return Err(LocationError::TooLarge);
        }
        if read_len == cap && meta.len() < cap {
            // Grew while being read past the cap. Silently truncating
            // would deliver half a file as if it were whole.
            return Err(LocationError::TooLarge);
        }
        // What was really delivered, if it turned out to be more than the
        // `stat` said. The charge can't come up short.
        self.charge_bytes(read_len.saturating_sub(meta.len()))?;
        Ok(buf)
    }

    /// Reads at most the first `max` bytes of a file under the root: what
    /// a header needs (`norte:location@0.2.0`, demo D2).
    ///
    /// Charges ONLY for what it returns, and `max` is also capped by
    /// `max_read_bytes`: a guest can't ask for "the first 4 GiB". A file
    /// shorter than `max` arrives whole, and that isn't an error — a guest
    /// that needs to know whether it was cut off has `stat`.
    ///
    /// # Errors
    ///
    /// [`LocationError`]'s: `Escapes` if the path escapes, `TypeMismatch`
    /// if it isn't a regular file, `Budget` if the session ran out.
    pub fn read_prefix(&self, rel: &[u8], max: u64) -> Result<Vec<u8>, LocationError> {
        self.charge_call()?;
        let (parent, name) = Self::split(rel)?;
        self.ensure_allowed(&Self::components(rel)?)?;
        let dir = self.dir_fd(&parent)?;
        let file = openat_read(dir.as_raw_fd(), name.as_ref())?;
        let meta = file.metadata().map_err(|e| from_io(&e))?;
        if !meta.is_file() {
            return Err(LocationError::TypeMismatch);
        }
        let cap = max.min(self.bounds.max_read_bytes);
        // Charged BEFORE reading, for what's about to be read at most: a
        // probe that fails can't be free (#240), and `st_size` can lie, so
        // the cap and not the size.
        self.charge_bytes(cap.min(meta.len()))?;
        let mut buf = Vec::with_capacity(usize::try_from(cap.min(meta.len())).unwrap_or(0));
        let read_len = std::io::Read::read_to_end(&mut std::io::Read::take(file, cap), &mut buf)
            .map_err(|e| from_io(&e))?;
        // What was really delivered above what was charged (a file that
        // grew after the `stat`): the charge can't come up short.
        let read_len = u64::try_from(read_len).unwrap_or(u64::MAX);
        self.charge_bytes(read_len.saturating_sub(cap.min(meta.len())))?;
        Ok(buf)
    }

    /// `lstat` of an entry under the root: the symlink is NOT followed.
    ///
    /// # Errors
    ///
    /// [`LocationError`]'s.
    ///
    /// # Panics
    ///
    /// Never: the `CString` for `"."` is a constant with no interior NUL.
    pub fn stat(&self, rel: &[u8]) -> Result<LocationMeta, LocationError> {
        self.charge_call()?;
        let (parent, name) = Self::split(rel)?;
        self.ensure_allowed(&Self::components(rel)?)?;
        let dir = self.dir_fd(&parent)?;
        match name {
            Some(name) => fstatat_nofollow(dir.as_raw_fd(), &name),
            // The root itself: `fstatat` with an empty name and
            // `AT_EMPTY_PATH` would be yet another syscall; a dirfd's `.`
            // already is it.
            None => fstatat_nofollow(dir.as_raw_fd(), &CString::new(".").expect("`.` has no NUL")),
        }
    }

    /// Lists a directory under the root. Names in raw bytes, `.` and `..`
    /// excluded.
    ///
    /// # Errors
    ///
    /// [`LocationError`]'s; `TypeMismatch` if `rel` isn't a directory.
    pub fn list(&self, rel: &[u8]) -> Result<Vec<LocationDirent>, LocationError> {
        self.charge_call()?;
        let comps = Self::components(rel)?;
        self.ensure_allowed(&comps)?;
        let dir = self.dir_fd(&comps)?;
        readdir_all(dir.as_raw_fd(), self.bounds.max_list_entries)
    }

    /// Charges for a call. Charged BEFORE doing the work and also when the
    /// work is about to fail: a probe that fails on purpose can't be free.
    fn charge_call(&self) -> Result<(), LocationError> {
        let mut spent = self.spent.lock().expect("spent lock is healthy");
        if spent.calls >= self.bounds.max_calls {
            return Err(LocationError::Budget);
        }
        spent.calls += 1;
        Ok(())
    }

    fn charge_bytes(&self, n: u64) -> Result<(), LocationError> {
        let mut spent = self.spent.lock().expect("spent lock is healthy");
        let total = spent.bytes.saturating_add(n);
        if total > self.bounds.max_total_bytes {
            return Err(LocationError::Budget);
        }
        spent.bytes = total;
        Ok(())
    }

    /// Splits a relative path into segments, resolving `.` and `..`
    /// LEXICALLY.
    ///
    /// Lexically and not through the kernel, on purpose: the two verdicts
    /// agree on the one thing that's promised — not escaping — because
    /// each resulting segment is opened the same way, under [`LocalRoot`].
    /// What the lexical version avoids is having to hand the kernel a path
    /// this module hasn't looked at.
    fn components(rel: &[u8]) -> Result<Vec<Segment>, LocationError> {
        if rel.first() == Some(&b'/') {
            return Err(LocationError::Escapes);
        }
        let mut out: Vec<Segment> = Vec::new();
        for comp in rel.split(|b| *b == b'/') {
            match comp {
                b"" | b"." => {}
                b".." => {
                    if out.pop().is_none() {
                        return Err(LocationError::Escapes);
                    }
                }
                other => out.push(Segment::new(other.to_vec()).map_err(|_| LocationError::Io)?),
            }
        }
        Ok(out)
    }

    /// The PARENT's components and the last name already as a `CString`.
    /// `None` = the path is the root itself.
    fn split(rel: &[u8]) -> Result<(Vec<Segment>, Option<CString>), LocationError> {
        let mut comps = Self::components(rel)?;
        let Some(last) = comps.pop() else {
            return Ok((comps, None));
        };
        let name = CString::new(last.as_bytes().to_vec()).map_err(|_| LocationError::Io)?;
        Ok((comps, Some(name)))
    }

    fn dir_fd(&self, comps: &[Segment]) -> Result<OwnedFd, LocationError> {
        self.root.resolve_dir(comps).map_err(|e| from_proto(&e))
    }
}

/// Opens a child for reading WITHOUT following a final symlink.
///
/// `O_NONBLOCK` and `O_NOCTTY` aren't decoration (#240): the type check
/// comes AFTER the open, and an `open(O_RDONLY)` on a FIFO with no writer
/// blocks forever. A hostile tarball can carry a FIFO named `.git/index`
/// — tar can hold them and norte extracts them —, and every repaint of
/// that page ate a thread from the blocking pool; wasmtime's epoch
/// interruption doesn't save you from that, because it only fires on wasm
/// instructions. On a regular file, `O_NONBLOCK` changes nothing about the
/// read.
fn openat_read(dir: RawFd, name: Option<&CString>) -> Result<std::fs::File, LocationError> {
    let Some(name) = name else {
        // Reading the root is reading a directory.
        return Err(LocationError::TypeMismatch);
    };
    #[allow(unsafe_code)]
    // SAFETY: `dir` is alive (the caller's `OwnedFd` holds it) and `name`
    // is a NUL-terminated CString alive for the whole call. The fd the
    // kernel returns has no owner until the `from_raw_fd` below.
    let raw = unsafe {
        libc::openat(
            dir,
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK | libc::O_NOCTTY,
        )
    };
    if raw < 0 {
        return Err(from_io(&std::io::Error::last_os_error()));
    }
    #[allow(unsafe_code)]
    // SAFETY: `raw` is a freshly opened, ownerless fd; `File` becomes its owner.
    Ok(unsafe { std::fs::File::from_raw_fd(raw) })
}

/// `fstatat` without following the final symlink.
fn fstatat_nofollow(dir: RawFd, name: &CString) -> Result<LocationMeta, LocationError> {
    let mut st: libc::stat = unsafe_zeroed_stat();
    #[allow(unsafe_code)]
    // SAFETY: `dir` is alive, `name` is NUL-terminated and alive, and `st`
    // is an owned, aligned `stat` the kernel fills in entirely.
    let rc = unsafe { libc::fstatat(dir, name.as_ptr(), &raw mut st, libc::AT_SYMLINK_NOFOLLOW) };
    if rc < 0 {
        return Err(from_io(&std::io::Error::last_os_error()));
    }
    Ok(meta_from_stat(&st))
}

/// A zeroed `libc::stat`, which is what the kernel expects to receive.
#[allow(unsafe_code)]
fn unsafe_zeroed_stat() -> libc::stat {
    // SAFETY: `libc::stat` is a POD of integers: the all-zeros pattern is a
    // valid value, and the kernel overwrites it entirely before it's read.
    unsafe { std::mem::zeroed() }
}

// `useless_conversion` is true ONLY on this target: `libc::stat`'s types
// change width across architectures (32-bit `time_t` still exists), and an
// `as` that truncates an inode turns two distinct files into the same one.
// The conversion stays.
#[allow(clippy::useless_conversion)]
fn meta_from_stat(st: &libc::stat) -> LocationMeta {
    let mode = st.st_mode;
    let kind = match mode & libc::S_IFMT {
        libc::S_IFREG => LocationKind::File,
        libc::S_IFDIR => LocationKind::Dir,
        libc::S_IFLNK => LocationKind::Symlink,
        _ => LocationKind::Other,
    };
    LocationMeta {
        kind,
        size: u64::try_from(st.st_size).unwrap_or(0),
        mtime_sec: i64::try_from(st.st_mtime).unwrap_or(0),
        mtime_nsec: u32::try_from(st.st_mtime_nsec).unwrap_or(0),
        ctime_sec: i64::try_from(st.st_ctime).unwrap_or(0),
        ctime_nsec: u32::try_from(st.st_ctime_nsec).unwrap_or(0),
        ino: u64::try_from(st.st_ino).unwrap_or(0),
        dev: u64::try_from(st.st_dev).unwrap_or(0),
        mode: u32::try_from(mode).unwrap_or(0),
    }
}

/// `readdir` over a dirfd, bounded.
fn readdir_all(dir: RawFd, max: u32) -> Result<Vec<LocationDirent>, LocationError> {
    // `fdopendir` takes ownership of the fd (`closedir` closes it), so it's
    // given a duplicate OPENED FOR READING: `LocalRoot`'s is `O_PATH`,
    // which doesn't work for iterating.
    #[allow(unsafe_code)]
    // SAFETY: `dir` is alive for the call; `"."` is a NUL-terminated
    // constant. The returned fd has no owner until the `fdopendir`.
    let raw = unsafe {
        libc::openat(
            dir,
            c".".as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if raw < 0 {
        return Err(from_io(&std::io::Error::last_os_error()));
    }
    #[allow(unsafe_code)]
    // SAFETY: `raw` is a freshly opened, ownerless directory fd;
    // `fdopendir` becomes its owner and `closedir` closes it at the end.
    let dirp = unsafe { libc::fdopendir(raw) };
    if dirp.is_null() {
        let err = std::io::Error::last_os_error();
        #[allow(unsafe_code)]
        // SAFETY: `fdopendir` failed, so the fd is still ours.
        unsafe {
            libc::close(raw)
        };
        return Err(from_io(&err));
    }
    let mut out = Vec::new();
    loop {
        // POSIX: `readdir` returns NULL both at the end and on failure,
        // and the only way to tell them apart is setting errno to 0
        // beforehand. Without this, a stale errno from any earlier call
        // would be read as a broken directory — or the other way around,
        // a real failure would pass as end-of-directory.
        clear_errno();
        #[allow(unsafe_code)]
        // SAFETY: `dirp` is a live DIR*, owned by this function until the
        // `closedir` below.
        let entry = unsafe { libc::readdir(dirp) };
        if entry.is_null() {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error().unwrap_or(0) != 0 {
                #[allow(unsafe_code)]
                // SAFETY: `dirp` is still alive and this is its only
                // release on this path.
                unsafe {
                    libc::closedir(dirp)
                };
                return Err(from_io(&err));
            }
            break;
        }
        #[allow(unsafe_code)]
        // SAFETY: `entry` is a valid pointer to a `dirent` owned by the
        // DIR*, alive until the next `readdir`, and only read here.
        let (name, d_type) = unsafe {
            let name = std::ffi::CStr::from_ptr((*entry).d_name.as_ptr())
                .to_bytes()
                .to_vec();
            (name, (*entry).d_type)
        };
        if name == b"." || name == b".." {
            continue;
        }
        out.push(LocationDirent {
            name,
            kind: kind_from_d_type(d_type),
        });
        if u32::try_from(out.len()).unwrap_or(u32::MAX) >= max {
            break;
        }
    }
    #[allow(unsafe_code)]
    // SAFETY: `dirp` is still alive and this is its only release.
    unsafe {
        libc::closedir(dirp)
    };
    Ok(out)
}

/// Sets `errno` to zero. POSIX requires it before a `readdir` whose NULL
/// has to be interpreted.
#[allow(unsafe_code)]
fn clear_errno() {
    // SAFETY: `errno` is thread-local and the pointer these two functions
    // return points at it; writing a 0 to it is the form POSIX defines.
    unsafe {
        #[cfg(target_os = "linux")]
        {
            *libc::__errno_location() = 0;
        }
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        {
            *libc::__error() = 0;
        }
    }
}

fn kind_from_d_type(d_type: u8) -> LocationKind {
    match d_type {
        libc::DT_REG => LocationKind::File,
        libc::DT_DIR => LocationKind::Dir,
        libc::DT_LNK => LocationKind::Symlink,
        _ => LocationKind::Other,
    }
}

fn from_io(e: &std::io::Error) -> LocationError {
    match e.raw_os_error() {
        Some(libc::ENOENT) => LocationError::NotFound,
        Some(libc::EACCES | libc::EPERM) => LocationError::Denied,
        // `ELOOP` = a symlink that isn't followed; `EXDEV` = what
        // `openat2(RESOLVE_BENEATH)` returns when the resolution escapes.
        Some(libc::ELOOP | libc::EXDEV) => LocationError::Escapes,
        Some(libc::EISDIR | libc::ENOTDIR) => LocationError::TypeMismatch,
        _ => LocationError::Io,
    }
}

fn from_proto(e: &norte_proto::Error) -> LocationError {
    use norte_proto::{ConflictKind, Error};
    match e {
        Error::NotFound => LocationError::NotFound,
        Error::PermissionDenied => LocationError::Denied,
        Error::Conflict {
            conflict: ConflictKind::EscapesRoot,
        } => LocationError::Escapes,
        Error::Conflict { .. } => LocationError::TypeMismatch,
        _ => LocationError::Io,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #240: a FIFO doesn't hang the reader.
    ///
    /// The type check comes after the open, so without `O_NONBLOCK` an
    /// `open(O_RDONLY)` on a FIFO with no writer stays there forever — one
    /// thread of the blocking pool per painted page, and wasmtime's epoch
    /// interruption never finds out.
    #[test]
    fn a_fifo_does_not_block_on_open() {
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("index");
        let c = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes().to_vec()).unwrap();
        #[allow(unsafe_code)]
        // SAFETY: `c` lives for the whole call and is a NUL-terminated path
        // inside a freshly created tempdir.
        let rc = unsafe { libc::mkfifo(c.as_ptr(), 0o600) };
        assert_eq!(rc, 0, "mkfifo: {}", std::io::Error::last_os_error());

        let root = ConfinedRoot::open(dir.path(), Bounds::default(), &[]).unwrap();
        // Without `O_NONBLOCK` this assert doesn't fail: it never ends.
        assert_eq!(root.read(b"index"), Err(LocationError::TypeMismatch));
    }

    /// `read_prefix` (norte:location 0.2.0): delivers and CHARGES at most
    /// `max` bytes. A column over a hundred videos costs a hundred headers
    /// and not a hundred videos, and the session's budget reflects it.
    #[test]
    fn read_prefix_delivers_and_charges_only_the_header() {
        let dir = tempfile::tempdir().unwrap();
        let content: Vec<u8> = (0..100u8).collect();
        std::fs::write(dir.path().join("track.mp3"), &content).unwrap();
        std::fs::create_dir(dir.path().join("folder")).unwrap();

        let bounds = Bounds {
            max_total_bytes: 60,
            ..Bounds::default()
        };
        let root = ConfinedRoot::open(dir.path(), bounds, &[]).unwrap();
        assert_eq!(
            root.read_prefix(b"track.mp3", 50).unwrap(),
            content[..50],
            "the first 50"
        );
        // A second 50-byte prefix doesn't fit in the session's 60: what
        // was charged was what was delivered, not the file's size.
        assert_eq!(
            root.read_prefix(b"track.mp3", 50),
            Err(LocationError::Budget)
        );

        let root = ConfinedRoot::open(dir.path(), Bounds::default(), &[]).unwrap();
        assert_eq!(
            root.read_prefix(b"track.mp3", 1000).unwrap(),
            content,
            "shorter than `max`: whole, no error"
        );
        assert_eq!(
            root.read_prefix(b"folder", 10),
            Err(LocationError::TypeMismatch)
        );
        assert_eq!(
            root.read_prefix(b"../outside", 10),
            Err(LocationError::Escapes)
        );
    }

    /// #238: a protected root falling INSIDE isn't crossed, not at one
    /// level nor at three.
    ///
    /// It's the real case and nothing hostile is needed to reach it: with
    /// the pane at `$XDG_CONFIG_HOME` the confined root was that
    /// directory, and `norte/` — the journal, the secrets, the connections
    /// — was underneath.
    #[test]
    fn a_protected_root_from_inside_is_not_traversed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("norte/deep")).unwrap();
        std::fs::write(dir.path().join("norte/secrets.age"), b"nope").unwrap();
        std::fs::write(dir.path().join("norte/deep/x"), b"nope-either").unwrap();
        std::fs::write(dir.path().join("free.txt"), b"yes").unwrap();
        let blocked = dir.path().join("norte");

        let root = ConfinedRoot::open(dir.path(), Bounds::default(), &[blocked]).unwrap();
        assert_eq!(
            root.read(b"free.txt").unwrap(),
            b"yes",
            "everything else reads fine"
        );
        assert_eq!(
            root.read(b"norte/secrets.age"),
            Err(LocationError::Denied),
            "one level"
        );
        assert_eq!(
            root.read(b"norte/deep/x"),
            Err(LocationError::Denied),
            "and three: EVERY prefix is checked, not just the destination"
        );
        assert_eq!(root.list(b"norte"), Err(LocationError::Denied));
        assert_eq!(root.stat(b"norte"), Err(LocationError::Denied));
        assert_eq!(
            root.stat(b"norte/secrets.age"),
            Err(LocationError::Denied),
            "not even confirming it exists"
        );
    }

    /// And the veto is by INODE, so a symlink pointing at the protected
    /// root makes no difference: comparing strings is exactly what a
    /// symlink goes around.
    #[test]
    fn a_symlink_to_the_protected_root_does_not_get_in_either() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("norte")).unwrap();
        std::fs::write(dir.path().join("norte/secrets.age"), b"nope").unwrap();
        std::os::unix::fs::symlink("norte", dir.path().join("shortcut")).unwrap();
        let blocked = dir.path().join("norte");

        let root = ConfinedRoot::open(dir.path(), Bounds::default(), &[blocked]).unwrap();
        assert_eq!(
            root.read(b"shortcut/secrets.age"),
            Err(LocationError::Denied),
            "the shortcut resolves to the same inode"
        );
    }

    /// The protected root ITSELF can't be opened. The caller already
    /// checks this by path; this checks it by inode, which is what a
    /// symlink can't fake.
    #[test]
    fn the_protected_root_cannot_be_opened_as_a_root() {
        let dir = tempfile::tempdir().unwrap();
        let itself = dir.path().to_path_buf();
        assert_eq!(
            ConfinedRoot::open(dir.path(), Bounds::default(), &[itself]).err(),
            Some(LocationError::Denied)
        );
    }

    /// A protected root that doesn't exist or is OUTSIDE costs nothing and
    /// vetoes nothing: the process declares its own once and many don't
    /// apply.
    #[test]
    fn an_absent_or_outside_protected_path_does_not_get_in_the_way() {
        let outside = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f"), b"yes").unwrap();
        let root = ConfinedRoot::open(
            dir.path(),
            Bounds::default(),
            &[
                outside.path().to_path_buf(),
                dir.path().join("does-not-exist"),
            ],
        )
        .unwrap();
        assert_eq!(root.read(b"f").unwrap(), b"yes");
    }

    #[test]
    fn a_dotdot_does_not_escape_the_root() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("inside.txt"), b"yes").unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        let root = ConfinedRoot::open(dir.path(), Bounds::default(), &[]).unwrap();
        assert!(
            root.read(b"sub/../inside.txt").is_ok(),
            "an INTERIOR `..` is legitimate"
        );
        assert_eq!(root.read(b"../outside.txt"), Err(LocationError::Escapes));
    }

    #[test]
    fn a_symlink_pointing_outside_is_not_followed() {
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret"), b"nope").unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path().join("secret"), dir.path().join("escape"))
            .unwrap();
        let root = ConfinedRoot::open(dir.path(), Bounds::default(), &[]).unwrap();
        assert!(root.read(b"escape").is_err(), "a symlink is not a backdoor");
        // Seeing it IS allowed: it's a directory entry like any other.
        assert_eq!(root.stat(b"escape").unwrap().kind, LocationKind::Symlink);
    }

    #[test]
    fn an_intermediate_symlink_that_escapes_is_rejected_by_the_root() {
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir(outside.path().join("d")).unwrap();
        std::fs::write(outside.path().join("d/secret"), b"nope").unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path().join("d"), dir.path().join("door")).unwrap();
        let root = ConfinedRoot::open(dir.path(), Bounds::default(), &[]).unwrap();
        assert!(root.read(b"door/secret").is_err());
    }

    #[test]
    fn an_absolute_path_is_not_relative() {
        let dir = tempfile::tempdir().unwrap();
        let root = ConfinedRoot::open(dir.path(), Bounds::default(), &[]).unwrap();
        assert_eq!(root.read(b"/etc/passwd"), Err(LocationError::Escapes));
    }

    #[test]
    fn each_cap_cuts_at_its_edge() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("big.bin"), vec![0u8; 4096]).unwrap();
        let bounds = Bounds {
            max_read_bytes: 1024,
            max_calls: 2,
            max_total_bytes: 2048,
            max_list_entries: 8,
        };
        let root = ConfinedRoot::open(dir.path(), bounds, &[]).unwrap();
        assert_eq!(root.read(b"big.bin"), Err(LocationError::TooLarge));
        // The CALL budget is spent even when the read fails: otherwise a
        // guest probes the tree for free by failing on purpose.
        root.stat(b"big.bin").ok();
        assert_eq!(root.stat(b"big.bin"), Err(LocationError::Budget));
    }

    #[test]
    fn the_byte_budget_cuts_the_session() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a"), vec![b'x'; 100]).unwrap();
        std::fs::write(dir.path().join("b"), vec![b'y'; 100]).unwrap();
        let bounds = Bounds {
            max_total_bytes: 150,
            ..Bounds::default()
        };
        let root = ConfinedRoot::open(dir.path(), bounds, &[]).unwrap();
        assert_eq!(root.read(b"a").unwrap().len(), 100);
        assert_eq!(root.read(b"b"), Err(LocationError::Budget));
    }

    #[test]
    fn stat_brings_what_git_stores_in_its_index() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f"), b"x").unwrap();
        let root = ConfinedRoot::open(dir.path(), Bounds::default(), &[]).unwrap();
        let m = root.stat(b"f").unwrap();
        assert_eq!(m.size, 1);
        assert_eq!(m.kind, LocationKind::File);
        assert!(
            m.ino != 0 && m.dev != 0,
            "git compares ino/dev, not just mtime"
        );
        assert!(m.mtime_sec > 0, "and mtime with nanoseconds");
    }

    #[test]
    fn list_gives_names_as_raw_bytes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"").unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        {
            use std::os::unix::ffi::OsStrExt as _;
            let weird = std::ffi::OsStr::from_bytes(b"no\xffutf8");
            std::fs::write(dir.path().join(weird), b"").unwrap();
        }
        let root = ConfinedRoot::open(dir.path(), Bounds::default(), &[]).unwrap();
        let mut names: Vec<Vec<u8>> = root
            .list(b"")
            .unwrap()
            .into_iter()
            .map(|e| e.name)
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec![b"a.txt".to_vec(), b"no\xffutf8".to_vec(), b"sub".to_vec()]
        );
        let sub = root.list(b"sub").unwrap();
        assert!(
            sub.is_empty(),
            "an empty directory lists empty, not failing"
        );
    }

    #[test]
    fn listing_a_file_is_type_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f"), b"").unwrap();
        let root = ConfinedRoot::open(dir.path(), Bounds::default(), &[]).unwrap();
        assert_eq!(root.list(b"f"), Err(LocationError::TypeMismatch));
    }

    #[test]
    fn reading_a_directory_is_type_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("d")).unwrap();
        let root = ConfinedRoot::open(dir.path(), Bounds::default(), &[]).unwrap();
        assert_eq!(root.read(b"d"), Err(LocationError::TypeMismatch));
    }
}
