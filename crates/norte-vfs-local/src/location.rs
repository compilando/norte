//! [`ConfinedRoot`]: reading UNDER a directory without being able to escape
//! it, with a budget.
//!
//! Exists for the plugin-host's `location` capability (ADR 0057): an
//! approved guest gets an opaque token, not a path, and reads what's under
//! the directory the pane is listing. None of this can live outside this
//! crate — hard rule 2 says only here is `std::fs` touched, and confinement
//! is the kernel's, not a string check: on unix `openat2(RESOLVE_BENEATH)`
//! (the mechanism that closed #164), on Windows `NtCreateFile` relative to a
//! directory handle, one component at a time (ADR 0158).
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
//!   governed on unix by `LocalRoot` with #164's criterion (followed if
//!   they don't escape); on Windows NO reparse point is crossed, which is
//!   the same verdict in the safe direction (ADR 0158).
//! - **Paid for per call, and also when it fails.** If an error didn't
//!   spend budget, probing the tree by failing on purpose would be free.
//! - **A protected root falling INSIDE isn't entered** (#238). Confining
//!   bounds from above and says nothing about what's below: with the root
//!   at `$XDG_CONFIG_HOME` — a directory a human lists without a second
//!   thought — the guest read `norte/secrets.age`, `norte/journal.db` and
//!   `norte/connections.toml`, and with the root at `/` it read the whole
//!   disk. The veto is by [`NodeId`] of each directory along the path
//!   (`(dev, ino)` on unix, volume serial + `FileId` on Windows), not by
//!   comparing strings: a symlink pointing at the protected root gives the
//!   same node.
//!
//! There's no `write` and there won't be: the capability is READ-ONLY.

use std::path::Path;
use std::sync::Mutex;

use norte_proto::Segment;
use norte_vfs::NodeId;

#[cfg(unix)]
mod unix;
#[cfg(unix)]
use unix as sys;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as sys;

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
///
/// On Windows the fields are what Git for Windows itself records, because
/// that is what a guest compares them against: `ino` and `dev` are 0 (its
/// `lstat` leaves them unused), `ctime` is the creation time, and `mode` is
/// synthesised from the attributes (`0o100644`, `0o100444` read-only,
/// `0o040755`, `0o120777`). No inode is invented from a `FileId`.
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
    /// Mode (permissions + type), exactly as the system gives it on unix;
    /// synthesised on Windows (see above).
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
    root: sys::Root,
    bounds: Bounds,
    spent: Mutex<Spent>,
    /// Identity of the protected roots that fall inside this root (#238).
    /// Resolved ONCE, at open time: they're directories of this process
    /// and don't move under our feet for the duration of a call.
    forbidden: Vec<NodeId>,
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
    /// `expect` is the node the caller observed with [`Self::identify`] when
    /// it decided this path was the root. Between that look and this `open` there's a
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
        expect: Option<NodeId>,
    ) -> Result<Self, LocationError> {
        let root = sys::Root::open(dir)?;
        let own_id = root.id();
        if expect.is_some_and(|expected| expected != own_id) {
            return Err(LocationError::Denied);
        }
        // Resolved by identity and not by path prefix: comparing strings is
        // what a symlink goes around, and the root opened here may have
        // arrived through one. Only a protected root that does not exist (or
        // is not a directory) is skipped: one that fails for any other
        // reason, or cannot be identified, refuses the whole root, because
        // skipping it would silently drop its veto.
        let mut forbidden = Vec::new();
        for p in protected {
            match sys::Root::open(p) {
                Ok(opened) => forbidden.push(opened.id()),
                Err(LocationError::NotFound | LocationError::TypeMismatch) => {}
                Err(_) => return Err(LocationError::Denied),
            }
        }
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

    /// The identity [`Self::open_verified`] compares against: `dir` opened
    /// the same way a root is, or `None` if it cannot be opened.
    ///
    /// BLOCKING: goes inside `spawn_blocking` (hard rule 2).
    ///
    /// ```
    /// # use norte_vfs_local::{Bounds, ConfinedRoot};
    /// let dir = tempfile::tempdir().unwrap();
    /// let seen = ConfinedRoot::identify(dir.path());
    /// assert!(seen.is_some());
    /// assert!(ConfinedRoot::open_verified(dir.path(), Bounds::default(), &[], seen).is_ok());
    /// ```
    #[must_use]
    pub fn identify(dir: &Path) -> Option<NodeId> {
        sys::Root::open(dir).ok().map(|r| r.id())
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
            let Ok(dir) = self.root.dir(&comps[..up_to]) else {
                // Doesn't resolve as a directory: either it doesn't exist,
                // or it's a file. In neither case is it a protected root
                // one could pass through, and the real error will come
                // from the caller.
                break;
            };
            let id = sys::dir_id(&dir)?;
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
        let file = self.open_file(rel)?;
        let meta = file.metadata().map_err(|e| sys::from_io(&e))?;
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
            .map_err(|e| sys::from_io(&e))?;
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
        let file = self.open_file(rel)?;
        let meta = file.metadata().map_err(|e| sys::from_io(&e))?;
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
            .map_err(|e| sys::from_io(&e))?;
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
    pub fn stat(&self, rel: &[u8]) -> Result<LocationMeta, LocationError> {
        self.charge_call()?;
        let mut comps = Self::components(rel)?;
        self.ensure_allowed(&comps)?;
        let name = comps.pop();
        let dir = self.root.dir(&comps)?;
        sys::stat(&dir, name.as_ref())
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
        let dir = self.root.dir(&comps)?;
        sys::list(&dir, self.bounds.max_list_entries)
    }

    /// Opens a file under the root for reading, the final component NOT
    /// followed. Reading the root itself is reading a directory.
    fn open_file(&self, rel: &[u8]) -> Result<std::fs::File, LocationError> {
        let mut comps = Self::components(rel)?;
        self.ensure_allowed(&comps)?;
        let name = comps.pop().ok_or(LocationError::TypeMismatch)?;
        let dir = self.root.dir(&comps)?;
        sys::open_read(&dir, &name)
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
    /// each resulting segment is opened the same way, confined by the
    /// platform's root (`LocalRoot` on unix, handle-relative on Windows).
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
    #[cfg(unix)]
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
    #[cfg(unix)]
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

    #[cfg(unix)]
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

    #[cfg(unix)]
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
        #[cfg(unix)]
        assert!(
            m.ino != 0 && m.dev != 0,
            "git compares ino/dev, not just mtime"
        );
        // Git for Windows records them as 0; a made-up inode would never
        // match its index.
        #[cfg(windows)]
        assert_eq!((m.ino, m.dev, m.mode), (0, 0, 0o100_644));
        assert!(m.mtime_sec > 0, "and mtime with nanoseconds");
        assert_eq!(root.stat(b"").unwrap().kind, LocationKind::Dir, "the root");
    }

    #[cfg(unix)]
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

    /// A junction, which any user can create, pointing OUT of the root.
    #[cfg(windows)]
    fn junction(link: &Path, target: &Path) {
        let status = std::process::Command::new("cmd")
            .arg("/C")
            .arg("mklink")
            .arg("/J")
            .arg(link)
            .arg(target)
            .stdout(std::process::Stdio::null())
            .status()
            .expect("cmd");
        assert!(status.success(), "mklink /J");
    }

    /// No reparse point is crossed on Windows, not even one that stays
    /// inside: the safe side of unix's "followed if it does not escape"
    /// (ADR 0158).
    #[cfg(windows)]
    #[test]
    fn a_junction_is_never_crossed_nor_read() {
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret"), b"nope").unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("inner")).unwrap();
        std::fs::write(dir.path().join("inner/f"), b"yes").unwrap();
        junction(&dir.path().join("door"), outside.path());
        junction(&dir.path().join("loop"), &dir.path().join("inner"));
        let root = ConfinedRoot::open(dir.path(), Bounds::default(), &[]).unwrap();
        assert_eq!(root.read(b"door/secret"), Err(LocationError::Escapes));
        assert_eq!(root.list(b"door"), Err(LocationError::Escapes));
        assert_eq!(root.read(b"loop/f"), Err(LocationError::Escapes));
        assert_eq!(root.read(b"door"), Err(LocationError::Escapes));
        assert_eq!(root.stat(b"door").unwrap().kind, LocationKind::Symlink);
        assert_eq!(root.read(b"inner/f").unwrap(), b"yes");
    }

    /// The veto is by `FileId`, so another spelling of the protected root —
    /// here its case — names the same node and is refused.
    #[cfg(windows)]
    #[test]
    fn a_protected_root_is_vetoed_by_file_id_on_windows() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("norte")).unwrap();
        std::fs::write(dir.path().join("norte/secrets.age"), b"nope").unwrap();
        let root =
            ConfinedRoot::open(dir.path(), Bounds::default(), &[dir.path().join("NORTE")]).unwrap();
        assert_eq!(
            root.read(b"norte/secrets.age"),
            Err(LocationError::Denied),
            "a different spelling of the same directory is the same node"
        );
    }

    /// A component the NT parser would split or reinterpret is refused
    /// rather than handed to `NtCreateFile`.
    #[cfg(windows)]
    #[test]
    fn separators_and_streams_do_not_reach_the_kernel() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("f"), b"x").unwrap();
        let root = ConfinedRoot::open(dir.path(), Bounds::default(), &[]).unwrap();
        assert_eq!(root.read(b"sub\\..\\..\\x"), Err(LocationError::Escapes));
        assert_eq!(root.read(b"f:stream"), Err(LocationError::Escapes));
        assert_eq!(
            root.read(b"C:/Windows/win.ini"),
            Err(LocationError::Escapes)
        );
    }

    #[cfg(windows)]
    #[test]
    fn list_gives_names_as_wtf8_bytes() {
        use std::os::windows::ffi::OsStringExt as _;
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"").unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        // A lone surrogate: a legal NTFS name that is not Unicode.
        let weird = std::ffi::OsString::from_wide(&[0x6E, 0xD800]);
        std::fs::write(dir.path().join(&weird), b"x").unwrap();
        let root = ConfinedRoot::open(dir.path(), Bounds::default(), &[]).unwrap();
        let mut names: Vec<Vec<u8>> = root
            .list(b"")
            .unwrap()
            .into_iter()
            .map(|e| e.name)
            .collect();
        names.sort();
        let weird_bytes = norte_vfs::wtf8::os_to_bytes(&weird);
        assert_eq!(
            names,
            vec![b"a.txt".to_vec(), weird_bytes.clone(), b"sub".to_vec()]
        );
        assert_eq!(root.read(&weird_bytes).unwrap(), b"x", "and it reads back");
        assert!(root.list(b"sub").unwrap().is_empty());
    }
}
