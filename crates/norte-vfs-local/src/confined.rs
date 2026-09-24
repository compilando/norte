//! Writing under a root without being able to escape it (#164, ADR 0054).
//!
//! The hole this closes: a sync validates its two roots and then composes
//! `dest_root + rel` step by step. If an INTERMEDIATE component of that
//! relative path is a symlink pointing outside, the write lands outside and
//! none of the three overlap checks sees it — they all reason about the
//! roots, and the roots are fine.
//!
//! **Composing the path and checking it before opening is TOCTOU by
//! construction**, so none is composed here: the caller opens the root
//! ONCE and from then on addresses relative segments. What's held is a
//! directory descriptor, and a descriptor can't be swapped for a symlink
//! between two syscalls.
//!
//! # What exactly is forbidden
//!
//! Escaping the root, and that's the whole rule — **not** "having
//! symlinks". A RELATIVE symlink pointing somewhere else inside the root
//! is followed, because forbidding that would break legitimate trees (an
//! ordinary `dst/data -> storage`) without gaining any security.
//!
//! An ABSOLUTE one is rejected even if its target falls inside. That's not
//! this module's decision: it's what `RESOLVE_BENEATH` does — to the
//! kernel an absolute path already starts outside the root —, and the
//! emulation walk copies it because the two branches have to give the SAME
//! verdict. Confinement that meant one thing with `openat2` and another
//! without it would not be a guarantee, it would be a kernel-version
//! lottery.
//!
//! # How, per platform
//!
//! | where | how |
//! | --- | --- |
//! | Linux ≥5.6 | `openat2(RESOLVE_BENEATH)` — the kernel guarantees it, step by step |
//! | Linux <5.6, seccomp (`ENOSYS`/`EPERM`), macOS | component-by-component walk: `openat(O_NOFOLLOW)` relative to the previous fd, and the `ELOOP` that gives IS the answer to "was it a symlink?" — absolute is rejected, relative is followed and checked for containment ON THE DESCRIPTOR |
//!
//! The two branches give the same verdict on everything that matters, with
//! one known difference in the safe direction: `openat2` rejects a
//! relative symlink that EXITS and re-enters (`../../root/inside`) because
//! it looks at the path, and the walk accepts it because it looks at where
//! it ends up. And [`is_beneath`]'s climb is best-effort under concurrent
//! renames, while `openat2` has no such window because the kernel resolves
//! it as one piece. `libpathrs` documents the same limitation for the same
//! emulation.
//! | Windows | there's no `openat`: this module doesn't exist there and `open_root` answers `Unsupported` |

use std::ffi::CString;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::Path;

use norte_proto::{ConflictKind, Entry, EntryKind, Error, Segment};

/// Ceiling on the levels the containment check climbs before giving up. A
/// tree more than 256 levels deep isn't anyone's file tree anymore.
const MAX_CLIMB: usize = 256;

/// An opened root, and the only place from which anything under it is
/// addressed.
///
/// The `fd` is the object: as long as it lives, it points at the directory
/// that was opened, even if someone renames or replaces the path it was
/// opened by.
#[derive(Debug)]
pub(crate) struct LocalRoot {
    fd: OwnedFd,
}

impl LocalRoot {
    /// Opens `dir` as a confined root.
    ///
    /// BLOCKING: goes inside `spawn_blocking` (hard rule 2).
    #[allow(unsafe_code)]
    pub(crate) fn open(dir: &Path) -> Result<Self, Error> {
        use std::os::unix::ffi::OsStrExt as _;
        let c = CString::new(dir.as_os_str().as_bytes()).map_err(|_| Error::InvalidPath)?;
        // SAFETY: `c` is a NUL-terminated CString alive for the whole call.
        // `O_PATH` neither reads nor writes anything: it only names the
        // node, which is all that's needed to use it as the dirfd for the
        // `*at` calls.
        let raw = unsafe {
            libc::open(
                c.as_ptr(),
                libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC,
            )
        };
        if raw < 0 {
            return Err(map_errno(&std::io::Error::last_os_error()));
        }
        // SAFETY: `raw` is a freshly opened, still ownerless fd; `OwnedFd`
        // becomes the only one that closes it.
        Ok(Self {
            fd: unsafe { OwnedFd::from_raw_fd(raw) },
        })
    }

    /// The PARENT directory of `rel`, opened under the root and without
    /// having been able to escape it. An empty `rel` is the root itself.
    ///
    /// Returns the parent's fd and the last segment, which is the name to
    /// operate on with a single-component `*at` — and a single component
    /// can't escape anywhere.
    pub(crate) fn parent_of<'a>(
        &self,
        rel: &'a [Segment],
    ) -> Result<(OwnedFd, &'a Segment), Error> {
        let (last, parents) = rel.split_last().ok_or(Error::InvalidPath)?;
        let fd = self.resolve_dir(parents)?;
        Ok((fd, last))
    }

    /// The root's fd, to identify it by `(dev, ino)` (#238).
    pub(crate) fn raw_fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }

    /// Opens the `parents` directory under the root, confined.
    pub(crate) fn resolve_dir(&self, parents: &[Segment]) -> Result<OwnedFd, Error> {
        if parents.is_empty() {
            return dup(self.fd.as_raw_fd());
        }
        #[cfg(target_os = "linux")]
        if !force_walk() {
            match openat2_beneath(self.fd.as_raw_fd(), parents) {
                // `ENOSYS` = kernel <5.6 or a seccomp filter that doesn't
                // let it through: the walk does the same thing, one
                // syscall per component.
                Err(e) if is_enosys(&e) => {}
                other => return other.map_err(|e| map_errno(&e)),
            }
        }
        walk_beneath(self.fd.as_raw_fd(), parents)
    }
}

/// `dup` of an fd, so that "the root itself" is returned with the same
/// type as any other resolved directory.
#[allow(unsafe_code)]
fn dup(fd: RawFd) -> Result<OwnedFd, Error> {
    // SAFETY: `fd` is alive (the caller's `OwnedFd` holds it) and
    // `F_DUPFD_CLOEXEC` returns a new fd nobody else owns.
    let raw = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) };
    if raw < 0 {
        return Err(map_errno(&std::io::Error::last_os_error()));
    }
    // SAFETY: `raw` is a freshly created, ownerless fd.
    Ok(unsafe { OwnedFd::from_raw_fd(raw) })
}

/// `openat2` with `RESOLVE_BENEATH`: the kernel rejects any resolution
/// that would escape `root`, intermediate symlink included, with nobody
/// having to check anything between two calls.
#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
fn openat2_beneath(root: RawFd, parents: &[Segment]) -> Result<OwnedFd, std::io::Error> {
    // `openat2`'s syscall number is 437 on every Linux architecture this
    // workspace compiles for; `libc` doesn't expose the constant for
    // x86_64-gnu.
    const SYS_OPENAT2: libc::c_long = 437;

    /// `struct open_how` from `<linux/openat2.h>`. `libc` declares it
    /// `#[non_exhaustive]`, so it can't be constructed from outside the
    /// crate; the ABI is three `u64`s and is FROZEN by design (the kernel
    /// extends it by adding fields at the end and checking the size it's
    /// given, which is exactly what the call below does).
    #[repr(C)]
    struct OpenHow {
        flags: u64,
        mode: u64,
        resolve: u64,
    }

    let joined = join(parents);
    let c = CString::new(joined).map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL))?;
    let how = OpenHow {
        flags: (libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC) as u64,
        mode: 0,
        // BENEATH forbids ESCAPING; symlinks that don't escape are
        // followed, which is what the emulation walk's containment check
        // does. NO_MAGICLINKS closes `/proc/*/fd/*`, which IS an escape
        // wearing an ordinary path's shape.
        resolve: libc::RESOLVE_BENEATH | libc::RESOLVE_NO_MAGICLINKS,
    };
    // SAFETY: `c` lives for the whole call; `how` is a complete, owned,
    // aligned `open_how`, and its exact size is passed as `openat2`'s
    // extensible ABI requires. The return value is checked before use.
    let raw = unsafe {
        libc::syscall(
            SYS_OPENAT2,
            root,
            c.as_ptr(),
            &raw const how,
            std::mem::size_of::<OpenHow>(),
        )
    };
    if raw < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let raw = RawFd::try_from(raw).map_err(|_| std::io::Error::from_raw_os_error(libc::EBADF))?;
    // SAFETY: `raw` is a freshly opened fd from the kernel, ownerless.
    Ok(unsafe { OwnedFd::from_raw_fd(raw) })
}

/// The walk: one `openat` per component, always relative to the previous
/// fd.
///
/// There's never a path to recompose, so the guarantee is the same as the
/// kernel's even though the mechanism differs. A component that's a
/// symlink is FOLLOWED and then checked to fall under the root — the
/// alternative (refusing to follow any symlink) would be stricter than
/// `RESOLVE_BENEATH` and would break legitimate trees.
///
/// **It's ALWAYS opened with `O_NOFOLLOW` and the symlink is discovered by
/// the `ELOOP` that produces**, never by asking beforehand whether the
/// name is a symlink. Asking first and opening after are two syscalls on a
/// NAME, and a substitution fits between the two: this phase's review
/// found exactly that — the component `lstat` had seen as a directory was
/// opened without `O_NOFOLLOW` and without a containment check, so a
/// switcheroo won in that gap would send the rest of the walk outside the
/// root. An `openat` that fails hasn't opened anything, and what's checked
/// afterward is always the already-opened DESCRIPTOR, never a name.
fn walk_beneath(root: RawFd, parents: &[Segment]) -> Result<OwnedFd, Error> {
    let root_id = node_id_of(root)?;
    let mut current = dup(root)?;
    for seg in parents {
        let name = CString::new(seg.as_bytes().to_vec()).map_err(|_| Error::InvalidPath)?;
        match openat_dir_nofollow(current.as_raw_fd(), &name) {
            Ok(next) => current = next,
            // With `O_NOFOLLOW` a final symlink comes out as `ELOOP`…
            // unless `O_DIRECTORY` is also set, in which case the kernel
            // prefers to answer `ENOTDIR` — which is also the legitimate
            // answer for an ordinary file in the middle of the path. So
            // both errnos mean "maybe it was a symlink", and the
            // `readlinkat` below is what breaks the tie: if it wasn't, the
            // original error stands.
            Err(e) if matches!(e.raw_os_error(), Some(libc::ELOOP | libc::ENOTDIR)) => {
                // An ABSOLUTE symlink is rejected even if it points
                // inside. That's what `RESOLVE_BENEATH` does — to the
                // kernel an absolute path already "starts" outside the
                // root — and the two branches have to give the same
                // verdict, or confinement would mean one thing on one
                // kernel and another on the next.
                match link_target_is_absolute(current.as_raw_fd(), &name)? {
                    // It wasn't a symlink: an ordinary file where the path
                    // asked for a directory. Its error is the one that
                    // stands.
                    None => return Err(map_errno(&e)),
                    Some(true) => {
                        return Err(Error::Conflict {
                            conflict: ConflictKind::EscapesRoot,
                        });
                    }
                    Some(false) => {}
                }
                // Follow it: here it IS opened without `O_NOFOLLOW`, and it
                // doesn't matter if between the `ELOOP` and this it gets
                // replaced by another symlink — what's checked next is
                // whatever descriptor comes out, not the name it was
                // requested by.
                let next = openat_dir(current.as_raw_fd(), &name)?;
                if !is_beneath(next.as_raw_fd(), root_id)? {
                    return Err(Error::Conflict {
                        conflict: ConflictKind::EscapesRoot,
                    });
                }
                current = next;
            }
            Err(e) => return Err(map_errno(&e)),
        }
    }
    Ok(current)
}

/// Does the RAW target of symlink `name` start with `/`?
///
/// The bytes are read as they are and nothing is resolved (rule 1): the
/// only question asked is whether it's absolute.
///
/// `Ok(None)` = `name` is NOT a symlink (`EINVAL` from `readlinkat`), which
/// is also how the `ENOTDIR` from an `openat(O_NOFOLLOW | O_DIRECTORY)` is
/// disambiguated: that errno is produced both by a symlink and by an
/// ordinary file, and only this tells them apart.
#[allow(unsafe_code)]
fn link_target_is_absolute(dir: RawFd, name: &CString) -> Result<Option<bool>, Error> {
    let mut buf = [0i8; 2];
    // SAFETY: `dir` is alive, `name` is NUL-terminated and alive, and `buf`
    // has room for the bytes requested. `readlinkat` does NOT NUL-terminate:
    // that's why only what it reports having written is looked at.
    let n = unsafe { libc::readlinkat(dir, name.as_ptr(), buf.as_mut_ptr().cast(), buf.len()) };
    if n < 0 {
        let e = std::io::Error::last_os_error();
        if e.raw_os_error() == Some(libc::EINVAL) {
            return Ok(None);
        }
        return Err(map_errno(&e));
    }
    Ok(Some(n > 0 && buf[0] == i8::try_from(b'/').unwrap_or(0)))
}

/// `openat` of a component as a directory, WITHOUT following a final
/// symlink.
///
/// Returns the raw `io::Error` and not the taxonomy: the caller needs to
/// distinguish the `ELOOP` of "this is a symlink" from the rest, and
/// `map_errno` deliberately melts them into a single verdict.
#[allow(unsafe_code)]
fn openat_dir_nofollow(dir: RawFd, name: &CString) -> Result<OwnedFd, std::io::Error> {
    // SAFETY: `dir` is alive and `name` is a NUL-terminated CString alive
    // for the whole call. `O_PATH` neither reads nor writes.
    let raw = unsafe {
        libc::openat(
            dir,
            name.as_ptr(),
            libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if raw < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `raw` is a freshly opened, ownerless fd.
    Ok(unsafe { OwnedFd::from_raw_fd(raw) })
}

/// `openat` of a component as a directory FOLLOWING the symlink, which is
/// what's wanted once it's known to be one and that its target is
/// relative. Whatever comes out is checked with [`is_beneath`] before
/// being used for anything.
#[allow(unsafe_code)]
fn openat_dir(dir: RawFd, name: &std::ffi::CStr) -> Result<OwnedFd, Error> {
    // SAFETY: `dir` is alive and `name` is a NUL-terminated CString alive
    // for the whole call. `O_PATH` neither reads nor writes.
    let raw = unsafe {
        libc::openat(
            dir,
            name.as_ptr(),
            libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if raw < 0 {
        return Err(map_errno(&std::io::Error::last_os_error()));
    }
    // SAFETY: `raw` is a freshly opened, ownerless fd.
    Ok(unsafe { OwnedFd::from_raw_fd(raw) })
}

/// Can `root_id` be reached from `fd` by climbing `..`?
///
/// The check is done on the already-opened DESCRIPTOR, not on a path, so
/// what's validated is exactly the object that's going to be used
/// afterward, not a name that could point at something else by the time
/// it's opened.
///
/// **The climb itself is best-effort under concurrent renames**, and that
/// has to be said instead of left to assume: if someone moves the
/// already-opened directory OUTSIDE the root between this computation and
/// the `mkdirat`/`openat` that comes after, the verdict was computed over
/// a tree that no longer is. It takes write permission INSIDE the root to
/// attempt it, and `openat2` — which is the stock path on Linux — has no
/// such window because the kernel resolves it as one piece. `libpathrs`
/// documents the same limitation for the same emulation.
fn is_beneath(fd: RawFd, root_id: (u64, u64)) -> Result<bool, Error> {
    if node_id_of(fd)? == root_id {
        return Ok(true);
    }
    let dotdot = c"..";
    let mut current = dup(fd)?;
    for _ in 0..MAX_CLIMB {
        let parent = openat_dir(current.as_raw_fd(), dotdot)?;
        let parent_id = node_id_of(parent.as_raw_fd())?;
        if parent_id == root_id {
            return Ok(true);
        }
        // The filesystem root is its own parent: the road ends here.
        if parent_id == node_id_of(current.as_raw_fd())? {
            return Ok(false);
        }
        current = parent;
    }
    Ok(false)
}

/// `(dev, ino)` of an open fd.
#[allow(unsafe_code)]
pub(crate) fn node_id_of(fd: RawFd) -> Result<(u64, u64), Error> {
    let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: `fd` is alive and `st` is an owned, aligned `stat` that the
    // call fills in entirely. It's only read after checking the return
    // value.
    //
    // `fstat` on an `O_PATH` fd is one of the few operations `open(2)`'s
    // documentation explicitly allows on it.
    let rc = unsafe { libc::fstat(fd, st.as_mut_ptr()) };
    if rc != 0 {
        return Err(map_errno(&std::io::Error::last_os_error()));
    }
    // SAFETY: `fstat` returned 0, so it left `st` initialized.
    let st = unsafe { st.assume_init() };
    #[allow(clippy::useless_conversion)] // `dev_t`/`ino_t` vary by platform
    Ok((u64::from(st.st_dev), u64::try_from(st.st_ino).unwrap_or(0)))
}

/// The segments joined by `/`, which is the only thing `openat2` knows how
/// to receive. This isn't "composing a path": resolution stays relative to
/// the root's fd and it's the kernel that prevents escaping it.
#[cfg(target_os = "linux")]
fn join(parents: &[Segment]) -> Vec<u8> {
    let mut out = Vec::new();
    for (i, seg) in parents.iter().enumerate() {
        if i > 0 {
            out.push(b'/');
        }
        out.extend_from_slice(seg.as_bytes());
    }
    out
}

/// Does this error say `openat2` isn't available?
///
/// `ENOSYS` is a kernel <5.6. `EPERM` is what a seccomp filter that
/// doesn't know the syscall answers (Docker's default profile, for years),
/// and that's why it counts: it's the SAME situation — no `openat2` —
/// stated another way. A real `EPERM` from another cause landing here
/// opens nothing: the walk confines just the same and will fail again with
/// the same `EPERM` if it really was one.
#[cfg(target_os = "linux")]
fn is_enosys(e: &std::io::Error) -> bool {
    e.raw_os_error() == Some(libc::ENOSYS) || e.raw_os_error() == Some(libc::EPERM)
}

/// Translates the errno of a confined resolution.
///
/// `EXDEV` is what `openat2(RESOLVE_BENEATH)` answers when the resolution
/// would have escaped, and `ELOOP` is what an `O_NOFOLLOW` answers: the two
/// are the same verdict and are NOT `NotFound`, because a caller that sees
/// `NotFound` responds by creating the parent — exactly the operation this
/// exists to prevent.
pub(crate) fn map_errno(e: &std::io::Error) -> Error {
    match e.raw_os_error() {
        Some(libc::EXDEV | libc::ELOOP) => Error::Conflict {
            conflict: ConflictKind::EscapesRoot,
        },
        _ => crate::provider::map_io(e),
    }
}

/// Test seam: forces the walk even when the kernel has `openat2`, so the
/// emulation path is exercised on the same machine as the other one.
#[cfg(target_os = "linux")]
fn force_walk() -> bool {
    FORCE_WALK.load(std::sync::atomic::Ordering::Relaxed)
}

#[cfg(target_os = "linux")]
static FORCE_WALK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Guard that forces the walk while it lives (test seam).
#[cfg(target_os = "linux")]
#[doc(hidden)]
#[derive(Debug)]
pub struct ForceComponentWalk(());

#[cfg(target_os = "linux")]
impl ForceComponentWalk {
    pub(crate) fn new() -> Self {
        FORCE_WALK.store(true, std::sync::atomic::Ordering::Relaxed);
        Self(())
    }
}

#[cfg(target_os = "linux")]
impl Drop for ForceComponentWalk {
    fn drop(&mut self) {
        FORCE_WALK.store(false, std::sync::atomic::Ordering::Relaxed);
    }
}

// ---------- operations under the root ----------

impl LocalRoot {
    /// `mkdir` of `rel` under the root.
    #[allow(unsafe_code)]
    pub(crate) fn mkdir(&self, rel: &[Segment]) -> Result<(), Error> {
        let (dir, name) = self.parent_of(rel)?;
        let c = cstring(name)?;
        // SAFETY: `dir` lives for the whole call and `c` is a
        // NUL-terminated CString alive too. `0o777` gets trimmed by the
        // process umask, just like `std::fs::create_dir` does.
        let rc = unsafe { libc::mkdirat(dir.as_raw_fd(), c.as_ptr(), 0o777) };
        if rc != 0 {
            let e = std::io::Error::last_os_error();
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                return Err(Error::Conflict {
                    conflict: ConflictKind::Exists,
                });
            }
            return Err(map_errno(&e));
        }
        Ok(())
    }

    /// `symlink` of `rel` under the root, pointing at `target`.
    ///
    /// What's confined is WHERE the link lands: the `symlinkat` goes
    /// against the already-resolved parent descriptor, so an intermediate
    /// component that's a bridge to the outside can't take it along.
    /// WHERE it points is untouched — the source's bytes are copied as
    /// they are, like `Provider::symlink` does —: trimming the target
    /// would be inventing a different link from the one being copied.
    #[allow(unsafe_code)]
    pub(crate) fn symlink(&self, rel: &[Segment], target: &[u8]) -> Result<(), Error> {
        let (dir, name) = self.parent_of(rel)?;
        let name = cstring(name)?;
        // A target with a NUL inside isn't a path any unix can hold.
        let target = CString::new(target.to_vec()).map_err(|_| Error::InvalidPath)?;
        // SAFETY: `dir` lives for the whole call and both CStrings are
        // NUL-terminated and alive too.
        let rc = unsafe { libc::symlinkat(target.as_ptr(), dir.as_raw_fd(), name.as_ptr()) };
        if rc != 0 {
            let e = std::io::Error::last_os_error();
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                return Err(Error::Conflict {
                    conflict: ConflictKind::Exists,
                });
            }
            return Err(map_errno(&e));
        }
        Ok(())
    }

    /// `lstat` of `rel` under the root: describes the LINK, never its
    /// target (same contract as `Provider::stat`).
    #[allow(unsafe_code)]
    pub(crate) fn stat(&self, rel: &[Segment], path: norte_proto::VPath) -> Result<Entry, Error> {
        let (dir, name) = self.parent_of(rel)?;
        let c = cstring(name)?;
        let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: `dir` and `c` live for the call; `st` is an owned,
        // aligned `stat` filled in entirely. Only read after the 0.
        let rc = unsafe {
            libc::fstatat(
                dir.as_raw_fd(),
                c.as_ptr(),
                st.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if rc != 0 {
            return Err(map_errno(&std::io::Error::last_os_error()));
        }
        // SAFETY: `fstatat` returned 0, so it left `st` initialized.
        let st = unsafe { st.assume_init() };
        Ok(entry_from_stat(path, &st))
    }

    /// `(dev, ino)` of `rel` under the root, without following symlinks —
    /// the LINK's identity, consistent with [`Self::stat`].
    ///
    /// It's the same `fstatat` as `stat`, and deliberately doesn't reuse
    /// it: `stat` returns an [`Entry`], which needs the `VPath` to name
    /// it, and here there's nothing to name. What's wanted is the pair of
    /// numbers.
    ///
    /// The inode goes in via [`u128::from`] and not via a padded
    /// `try_from`: this is PERSISTED as identity and compared days later,
    /// so a padding value would make two distinct nodes compare equal and
    /// authorize a deletion. The conversion is infallible for any unix
    /// `ino_t`, so there's no degraded case to invent — which is better
    /// than having one and picking it a sentinel.
    #[allow(unsafe_code)]
    pub(crate) fn node_id(&self, rel: &[Segment]) -> Result<norte_vfs::NodeId, Error> {
        let (dir, name) = self.parent_of(rel)?;
        let c = cstring(name)?;
        let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: `dir` and `c` live for the call; `st` is an owned,
        // aligned `stat` filled in entirely. Only read after the 0.
        let rc = unsafe {
            libc::fstatat(
                dir.as_raw_fd(),
                c.as_ptr(),
                st.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if rc != 0 {
            return Err(map_errno(&std::io::Error::last_os_error()));
        }
        // SAFETY: `fstatat` returned 0, so it left `st` initialized.
        let st = unsafe { st.assume_init() };
        #[allow(clippy::useless_conversion)] // `dev_t`/`ino_t` vary by platform
        Ok(norte_vfs::NodeId {
            volume: u64::from(st.st_dev),
            index: u128::from(st.st_ino),
        })
    }

    /// `unlinkat` of the LEAF in `rel`, under the root (#218).
    ///
    /// WITHOUT `AT_REMOVEDIR`: what this deletes is a leaf about to be
    /// replaced, and a directory in its place is `TypeMismatch` — an
    /// answer, not a policy. Asking `unlinkat` to delete a dir without the
    /// flag returns `EISDIR`, which is exactly the right error and arrives
    /// without having touched anything.
    #[allow(unsafe_code)]
    pub(crate) fn remove(&self, rel: &[Segment]) -> Result<(), Error> {
        let (dir, name) = self.parent_of(rel)?;
        let c = cstring(name)?;
        // SAFETY: `dir` lives for the whole call and `c` is a
        // NUL-terminated CString alive too.
        let rc = unsafe { libc::unlinkat(dir.as_raw_fd(), c.as_ptr(), 0) };
        if rc != 0 {
            return Err(map_errno(&std::io::Error::last_os_error()));
        }
        Ok(())
    }

    /// The digest of the first `len` bytes of `rel`'s partial, opened BY
    /// THE DESCRIPTOR of the already-resolved directory.
    ///
    /// The SAME checks as [`Self::open_resumable`] — `O_NOFOLLOW`, regular
    /// file, single link, ours —: verifying the prefix of a file that
    /// isn't the one we're about to continue verifies nothing.
    #[allow(unsafe_code)]
    pub(crate) fn partial_digest(
        &self,
        rel: &[Segment],
        len: u64,
    ) -> Result<Option<[u8; 32]>, Error> {
        use std::io::Read as _;

        use sha2::{Digest as _, Sha256};
        let (dir, name) = self.parent_of(rel)?;
        let staging_name = CString::new(crate::provider::stable_partial_name(name.as_bytes()))
            .map_err(|_| Error::InvalidPath)?;
        // SAFETY: `dir` lives for the call and `staging_name` is a
        // NUL-terminated CString alive too. Read-only, and no `O_CREAT`: if
        // it's not there, there's no digest and the caller degrades to
        // `Length`.
        let raw = unsafe {
            libc::openat(
                dir.as_raw_fd(),
                staging_name.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
            )
        };
        if raw < 0 {
            let e = std::io::Error::last_os_error();
            if e.kind() == std::io::ErrorKind::NotFound {
                return Ok(None);
            }
            return Err(map_errno(&e));
        }
        // SAFETY: `raw` is a freshly opened, ownerless fd; `File` becomes its owner.
        let file = unsafe { <std::fs::File as std::os::fd::FromRawFd>::from_raw_fd(raw) };
        let st = fstat_of(&file)?;
        // SAFETY: `geteuid` takes no pointers and can't fail.
        if st.st_mode & libc::S_IFMT != libc::S_IFREG
            || st.st_nlink != 1
            || st.st_uid != unsafe { libc::geteuid() }
        {
            // Not the partial we left behind: no digest, and the caller
            // degrades. Rejecting it on resume is `open_resumable`'s job.
            return Ok(None);
        }
        let mut reader = file.take(len);
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 64 * 1024];
        let mut seen: u64 = 0;
        loop {
            let n = reader
                .read(&mut buf)
                .map_err(|e| crate::provider::map_io(&e))?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            seen += n as u64;
        }
        if seen < len {
            return Ok(None);
        }
        Ok(Some(hasher.finalize().into()))
    }

    /// `unlinkat(AT_REMOVEDIR)` of the EMPTY directory in `rel` (#296).
    #[allow(unsafe_code)]
    pub(crate) fn rmdir(&self, rel: &[Segment]) -> Result<(), Error> {
        let (dir, name) = self.parent_of(rel)?;
        let c = cstring(name)?;
        // SAFETY: `dir` lives for the whole call and `c` is a
        // NUL-terminated CString alive too.
        let rc = unsafe { libc::unlinkat(dir.as_raw_fd(), c.as_ptr(), libc::AT_REMOVEDIR) };
        if rc != 0 {
            return Err(map_errno(&std::io::Error::last_os_error()));
        }
        Ok(())
    }

    /// Opens a sink for `rel`: the staging is created with `openat` in the
    /// already-resolved directory and published with `renameat` on that
    /// SAME descriptor, so publishing is confined just like the write is.
    /// Between opening and publishing nobody can slip in a symlink that
    /// sends the rename somewhere else, because there's no path left to
    /// resolve again.
    pub(crate) fn open_write(&self, rel: &[Segment]) -> Result<ConfinedStaging, Error> {
        let (dir, name) = self.parent_of(rel)?;
        let final_name = cstring(name)?;
        // `Provider::write` promises a NEW file and says so AT OPEN TIME:
        // without this, an occupied destination wouldn't be known until
        // `commit`, i.e. after having transferred the whole file for
        // nothing.
        //
        // It's not THE guarantee — that's the publish's `RENAME_NOREPLACE`,
        // which has no window —, and that's why losing a race here loses
        // nothing: it comes out as a conflict a moment later.
        //
        // One nuance versus the by-path route: there, `collision_kind_for`
        // rereads the directory and distinguishes a CASE collision from a
        // normalization one; here it just answers `Exists`. Nothing in the
        // core decides based on that distinction (it's only rendered), and
        // rereading the directory would be walking what this path exists
        // to avoid walking.
        if !name_is_free(dir.as_raw_fd(), &final_name)? {
            return Err(Error::Conflict {
                conflict: ConflictKind::Exists,
            });
        }
        let staging_name = ephemeral_staging_name(name.as_bytes());
        let file = create_exclusive(dir.as_raw_fd(), &staging_name)?;
        Ok(ConfinedStaging {
            dir,
            file,
            staging: staging_name,
            final_name,
            estable: false,
        })
    }

    /// Like [`Self::open_write`], but with the STABLE staging a later
    /// resume can find again (#297, ADR 0012).
    ///
    /// The difference from the ephemeral one is only the NAME — derived
    /// from the final name's hash, with no pid or sequence — and that it's
    /// opened with `O_APPEND` without `O_EXCL`: if there were already bytes
    /// from an earlier attempt, it continues after them. Returns how many
    /// there were.
    ///
    /// This was needed because until #219 a leaf went WITHOUT confinement
    /// and therefore did resume; confining it left it without resume right
    /// where it matters most — a large file cut off by just one dropped
    /// link — and that was a regression, not a decision.
    #[allow(unsafe_code)]
    pub(crate) fn open_resumable(&self, rel: &[Segment]) -> Result<(ConfinedStaging, u64), Error> {
        let (dir, name) = self.parent_of(rel)?;
        let final_name = cstring(name)?;
        // The final destination must NOT exist yet: same contract as
        // `open_write`, and the collision policy belongs to the core.
        if !name_is_free(dir.as_raw_fd(), &final_name)? {
            return Err(Error::Conflict {
                conflict: ConflictKind::Exists,
            });
        }
        let staging_name = CString::new(crate::provider::stable_partial_name(name.as_bytes()))
            .map_err(|_| Error::InvalidPath)?;
        // SAFETY: `dir` lives for the call and `staging_name` is a
        // NUL-terminated CString alive too.
        //
        // No `O_EXCL` on purpose — it's REOPENED, which is what resuming
        // is about — and that's why the flags below and the `fstat` below
        // are needed. This name is PREDICTABLE: anyone who knows the
        // destination name can compute it, so the file on the other side
        // may have been put there by someone else.
        //
        // - `O_NOFOLLOW`: it's not a link.
        // - `O_NONBLOCK`: a FIFO planted with that name would hang the
        //   `openat` FOREVER inside the blocking pool, and the
        //   cancellation token can't interrupt an in-progress `openat`.
        //   Removed after checking it's a regular file.
        // - `0o600` and not `0o666`: what's created here is ours and
        //   nobody else's for as long as it lasts.
        let raw = unsafe {
            libc::openat(
                dir.as_raw_fd(),
                staging_name.as_ptr(),
                libc::O_WRONLY
                    | libc::O_CREAT
                    | libc::O_APPEND
                    | libc::O_NOFOLLOW
                    | libc::O_NONBLOCK
                    | libc::O_CLOEXEC,
                0o600,
            )
        };
        if raw < 0 {
            return Err(map_errno(&std::io::Error::last_os_error()));
        }
        // SAFETY: `raw` is a freshly opened, ownerless fd; `File` becomes its owner.
        let file = unsafe { <std::fs::File as std::os::fd::FromRawFd>::from_raw_fd(raw) };
        // **And now what was opened gets CHECKED**, which was the missing
        // piece. `O_NOFOLLOW` rules out a link and nothing more: a regular
        // file of an attacker's, a FIFO or a hardlink to a victim's file
        // all pass through that door. Resuming over any of the three
        // publishes under the legitimate name an inode that isn't ours —
        // with its content, its owner and its permissions — or appends our
        // bytes OUTSIDE the approved root, which is exactly what
        // confinement exists to prevent.
        let st = fstat_of(&file)?;
        if st.st_mode & libc::S_IFMT != libc::S_IFREG {
            return Err(Error::Conflict {
                conflict: ConflictKind::TypeMismatch,
            });
        }
        // A hardlink: our bytes would also go to the other name, which may
        // be outside the root.
        if st.st_nlink != 1 {
            return Err(Error::Conflict {
                conflict: ConflictKind::EscapesRoot,
            });
        }
        // SAFETY: `geteuid` takes no pointers and can't fail.
        if st.st_uid != unsafe { libc::geteuid() } {
            return Err(Error::Conflict {
                conflict: ConflictKind::EscapesRoot,
            });
        }
        // Having verified it's a regular file of ours, `O_NONBLOCK` no
        // longer serves any purpose; it's removed so as not to pass a
        // flag another layer isn't expecting down to it.
        remove_nonblock(&file)?;
        let already = u64::try_from(st.st_size).unwrap_or(0);
        Ok((
            ConfinedStaging {
                dir,
                file,
                staging: staging_name,
                final_name,
                estable: true,
            },
            already,
        ))
    }
}

/// A staging file opened under a confined root, with everything its
/// publication needs: the directory's descriptor, the temporary name and
/// the final one.
#[derive(Debug)]
pub(crate) struct ConfinedStaging {
    pub(crate) dir: OwnedFd,
    pub(crate) file: std::fs::File,
    pub(crate) staging: CString,
    pub(crate) final_name: CString,
    /// The staging's name is the STABLE one, i.e. rediscoverable by a
    /// later resume. It's what decides whether `keep` preserves it or
    /// deletes it (#297).
    pub(crate) estable: bool,
}

/// Publishes the staging under its final name, no-replace and on the same
/// directory descriptor.
#[allow(unsafe_code)]
pub(crate) fn publish(dir: RawFd, staging: &CString, final_name: &CString) -> Result<(), Error> {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: `dir` is alive and both CStrings are NUL-terminated and
        // alive. `RENAME_NOREPLACE` makes the rename ITSELF detect the
        // collision, with no window between checking and renaming.
        let rc = unsafe {
            libc::renameat2(
                dir,
                staging.as_ptr(),
                dir,
                final_name.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        if rc == 0 {
            return Ok(());
        }
        let e = std::io::Error::last_os_error();
        // `EINVAL`/`ENOSYS`/`EOPNOTSUPP`: a filesystem without no-replace
        // (some FUSE, old NFS). Degrades to a plain `renameat` preceded by
        // a `faccessat`, which is the window M0 already documents — and is
        // still confined, which is what this module guarantees.
        if !matches!(
            e.raw_os_error(),
            Some(libc::EINVAL | libc::ENOSYS | libc::EOPNOTSUPP)
        ) {
            return Err(rename_error(&e));
        }
    }
    plain_rename(dir, staging, final_name)
}

/// Is that name FREE in this directory? Looks at the NODE, not at what it
/// points to: a broken symlink occupies the name just like a file does.
///
/// `Ok(false)` = occupied. `Ok(true)` = free, and only the `ENOENT` says
/// so: any other errno is "I don't know" and comes out as `Err`, because
/// whoever asks does so right before a rename that OVERWRITES. The
/// previous version used `faccessat(F_OK, AT_SYMLINK_NOFOLLOW)` and read
/// any failure as "free", which is fail-OPEN: that flag needs
/// `faccessat2` (kernel 5.8+) and on an old kernel, or a libc that doesn't
/// use it, it answers `EINVAL` forever — so the name always looked free
/// and the degraded rename destroyed whatever was there. This phase's
/// security review found it.
#[allow(unsafe_code)]
fn name_is_free(dir: RawFd, name: &CString) -> Result<bool, Error> {
    let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: `dir` is alive, `name` is NUL-terminated and alive, and `st`
    // is an owned, aligned `stat`. It would only be read after a 0, and
    // here it's not even read.
    let rc = unsafe {
        libc::fstatat(
            dir,
            name.as_ptr(),
            st.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if rc == 0 {
        return Ok(false);
    }
    let e = std::io::Error::last_os_error();
    if e.kind() == std::io::ErrorKind::NotFound {
        return Ok(true);
    }
    Err(map_errno(&e))
}

/// Publishes without being able to overwrite, where `renameat2` isn't
/// available.
///
/// **First `linkat` + `unlinkat` is tried, which IS atomic no-replace and
/// portable.** `link` fails with `EEXIST` if the destination exists, and
/// the kernel decides that with no window at all — which is the whole
/// property. Only if the filesystem can't link (`EPERM`/`EOPNOTSUPP`/
/// `EMLINK`, and `EXDEV` can't happen because both names hang off the SAME
/// descriptor) does it fall back to a plain `renameat` preceded by the
/// check, which is the window M0 already documents.
///
/// Matters more than it looks: on macOS there's no `renameat2`, so this
/// was the ONLY publication path in the whole module, and the security
/// review flagged that the upfront check left every macOS publication with
/// a gap where someone else's file gets overwritten without a word.
#[allow(unsafe_code)]
fn plain_rename(dir: RawFd, staging: &CString, final_name: &CString) -> Result<(), Error> {
    // SAFETY: `dir` is alive and both CStrings are NUL-terminated and
    // alive. Without `AT_SYMLINK_FOLLOW`: the staging is linked, never
    // whatever it might have pointed to.
    let rc = unsafe { libc::linkat(dir, staging.as_ptr(), dir, final_name.as_ptr(), 0) };
    if rc == 0 {
        // Published. The staging is now spare: deleting it is best-effort
        // because the file is ALREADY in place and failing here would be
        // failing after having succeeded.
        let _ = discard(dir, staging);
        return Ok(());
    }
    let e = std::io::Error::last_os_error();
    if e.kind() == std::io::ErrorKind::AlreadyExists {
        return Err(Error::Conflict {
            conflict: ConflictKind::Exists,
        });
    }
    if !matches!(
        e.raw_os_error(),
        Some(libc::EPERM | libc::EOPNOTSUPP | libc::EMLINK)
    ) {
        return Err(rename_error(&e));
    }
    // No hardlinks: the plain rename is what's left, and its upfront
    // check. `name_is_free` fails when in doubt, so a strange errno is
    // NOT read as "free" and nothing gets overwritten out of not knowing.
    if !name_is_free(dir, final_name)? {
        return Err(Error::Conflict {
            conflict: ConflictKind::Exists,
        });
    }
    // SAFETY: `dir` is alive and both CStrings are NUL-terminated and alive.
    let rc = unsafe { libc::renameat(dir, staging.as_ptr(), dir, final_name.as_ptr()) };
    if rc == 0 {
        Ok(())
    } else {
        Err(rename_error(&std::io::Error::last_os_error()))
    }
}

/// Deletes the staging. A staging that's already gone isn't an error.
#[allow(unsafe_code)]
pub(crate) fn discard(dir: RawFd, staging: &CString) -> Result<(), Error> {
    // SAFETY: `dir` is alive and `staging` is NUL-terminated and alive.
    let rc = unsafe { libc::unlinkat(dir, staging.as_ptr(), 0) };
    if rc == 0 {
        return Ok(());
    }
    let e = std::io::Error::last_os_error();
    if e.kind() == std::io::ErrorKind::NotFound {
        return Ok(());
    }
    Err(map_errno(&e))
}

/// Creates the staging exclusively: `O_EXCL` so two writes never share
/// one, `O_NOFOLLOW` so a symlink planted with that name can't redirect
/// the creation.
#[allow(unsafe_code)]
fn create_exclusive(dir: RawFd, name: &CString) -> Result<std::fs::File, Error> {
    // SAFETY: `dir` is alive and `name` is NUL-terminated and alive. The
    // mode gets trimmed by the umask, same as in `std::fs::File::create`.
    let raw = unsafe {
        libc::openat(
            dir,
            name.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o666,
        )
    };
    if raw < 0 {
        return Err(map_errno(&std::io::Error::last_os_error()));
    }
    // SAFETY: `raw` is a freshly opened, ownerless fd; `File` becomes its owner.
    Ok(unsafe { <std::fs::File as std::os::fd::FromRawFd>::from_raw_fd(raw) })
}

/// Opens the STABLE staging of a PATH with the same checks as
/// [`LocalRoot::open_resumable`] (#298), and returns how many bytes there
/// were.
///
/// The by-path route has no directory descriptor to `openat` against —
/// confinement is a separate matter, and it's #219 —, but the set of
/// checks on what got opened is exactly the same, because the reason is
/// the same: the staging's name derives from the destination name's hash,
/// so **anyone who knows where we're about to copy to can compute it**,
/// and the file on the other side may have been put there by someone
/// else.
///
/// And they're checked on the OPEN FILE, not on the path: a prior `lstat`
/// answers about what was there, not about what got opened.
#[allow(unsafe_code)]
pub(crate) fn opens_staging_estable(path: &std::path::Path) -> Result<(std::fs::File, u64), Error> {
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};

    // No `O_EXCL` on purpose — it's REOPENED, which is what resuming is
    // about — and that's why the flags here and the checks below are
    // needed:
    //
    // - `O_NOFOLLOW`: it's not a link. A planted
    //   `.norte-partial.<hash> -> /etc/passwd` would receive our bytes and
    //   then `commit` would publish THAT inode under the legitimate name.
    // - `O_NONBLOCK`: a FIFO planted with that name hangs the `open`
    //   FOREVER inside the blocking pool, and the cancellation token can't
    //   interrupt an in-progress `open`. Removed as soon as it's known to
    //   be a regular file.
    // - `0o600` and not the earlier `0o666`: what's created here is ours
    //   and nobody else's for as long as it lasts.
    let file = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .mode(0o600)
        .open(path)
        .map_err(|e| map_errno(&e))?;
    let md = file.metadata().map_err(|e| crate::provider::map_io(&e))?;
    if !md.file_type().is_file() {
        return Err(Error::Conflict {
            conflict: ConflictKind::TypeMismatch,
        });
    }
    // A hardlink to a victim's file: our bytes would also go to the other
    // name, wherever the attacker wants it.
    if md.nlink() != 1 {
        return Err(Error::Conflict {
            conflict: ConflictKind::EscapesRoot,
        });
    }
    // SAFETY: `geteuid` takes no pointers and can't fail.
    if md.uid() != unsafe { libc::geteuid() } {
        return Err(Error::Conflict {
            conflict: ConflictKind::EscapesRoot,
        });
    }
    remove_nonblock(&file)?;
    Ok((file, md.len()))
}

/// Opens a path's partial for READING with the same checks (#298):
/// verifying the prefix of a file that isn't the one about to be continued
/// verifies nothing.
///
/// `Ok(None)` is "there's no partial of ours": the caller degrades to
/// `Length`, and rejecting whatever's there on resume is
/// [`opens_staging_estable`]'s job.
#[allow(unsafe_code)]
pub(crate) fn opens_partial_verificado(
    path: &std::path::Path,
) -> Result<Option<std::fs::File>, Error> {
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};

    let file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
    {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(map_errno(&e)),
    };
    let md = file.metadata().map_err(|e| crate::provider::map_io(&e))?;
    // SAFETY: `geteuid` takes no pointers and can't fail.
    if !md.file_type().is_file() || md.nlink() != 1 || md.uid() != unsafe { libc::geteuid() } {
        return Ok(None);
    }
    remove_nonblock(&file)?;
    Ok(Some(file))
}

/// `fstat` of an already-open descriptor.
#[allow(unsafe_code)]
fn fstat_of(file: &std::fs::File) -> Result<libc::stat, Error> {
    use std::os::fd::AsRawFd as _;
    let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: `file` lives for the call and `st` is an owned, aligned
    // `stat` filled in entirely. Only read after the 0.
    let rc = unsafe { libc::fstat(file.as_raw_fd(), st.as_mut_ptr()) };
    if rc != 0 {
        return Err(map_errno(&std::io::Error::last_os_error()));
    }
    // SAFETY: `fstat` returned 0, so it left `st` initialized.
    Ok(unsafe { st.assume_init() })
}

/// Removes `O_NONBLOCK` from an already-open descriptor.
#[allow(unsafe_code)]
fn remove_nonblock(file: &std::fs::File) -> Result<(), Error> {
    use std::os::fd::AsRawFd as _;
    // SAFETY: `file` lives for both calls; `F_GETFL`/`F_SETFL` take no
    // pointers.
    let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
    if flags < 0 {
        return Err(map_errno(&std::io::Error::last_os_error()));
    }
    let rc = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFL, flags & !libc::O_NONBLOCK) };
    if rc < 0 {
        return Err(map_errno(&std::io::Error::last_os_error()));
    }
    Ok(())
}

/// Ephemeral staging name, with the SAME shape as the provider's
/// (`.norte-partial.<16 hex>.<pid>-<seq>`) so the partial sweeper keeps
/// recognizing it — this is another way of reaching the same files, not a
/// second convention.
fn ephemeral_staging_name(final_name: &[u8]) -> CString {
    let name = crate::provider::ephemeral_partial_name(final_name);
    CString::new(name).expect("the staging name is hex and dots")
}

/// An `Entry` from a raw `stat`, without going through `std::fs::Metadata`
/// (which would require a path that, deliberately, doesn't exist here).
fn entry_from_stat(path: norte_proto::VPath, st: &libc::stat) -> Entry {
    let (kind, size) = match st.st_mode & libc::S_IFMT {
        libc::S_IFLNK => (EntryKind::Symlink, None),
        libc::S_IFDIR => (EntryKind::Dir, None),
        libc::S_IFREG => (EntryKind::File, u64::try_from(st.st_size).ok()),
        _ => (EntryKind::Other, None),
    };
    Entry {
        attrs: std::collections::BTreeMap::new(),
        path,
        kind,
        size,
        mtime_ms: mtime_ms_of(st),
    }
}

/// mtime in UTC milliseconds from a `stat`.
fn mtime_ms_of(st: &libc::stat) -> Option<i64> {
    // The types of `st_mtime`/`st_mtime_nsec` change with the platform and
    // the libc, so the conversion is redundant ONLY on today's target.
    #[allow(clippy::useless_conversion)]
    let secs = i64::try_from(st.st_mtime).ok()?;
    #[allow(clippy::useless_conversion)]
    let nanos = i64::try_from(st.st_mtime_nsec).ok()?;
    secs.checked_mul(1000)?.checked_add(nanos / 1_000_000)
}

/// A segment as a `CString`. A segment with a NUL inside isn't a name any
/// unix filesystem can hold.
fn cstring(seg: &Segment) -> Result<CString, Error> {
    CString::new(seg.as_bytes().to_vec()).map_err(|_| Error::InvalidPath)
}

/// The error from a publication rename: a destination that already exists
/// is `Conflict`, and the rest goes through the usual taxonomy.
fn rename_error(e: &std::io::Error) -> Error {
    if e.kind() == std::io::ErrorKind::AlreadyExists {
        return Error::Conflict {
            conflict: ConflictKind::Exists,
        };
    }
    map_errno(e)
}

// ---------- the handle the core sees ----------

/// [`norte_vfs::ConfinedRoot`] over a [`LocalRoot`].
///
/// The `Arc` isn't for sharing: the sink `write` returns outlives the
/// handle if the caller drops it before committing, and the directory
/// descriptor has to stay alive so the publication's `renameat` keeps
/// being the same directory and not a path resolved all over again.
#[derive(Debug)]
pub(crate) struct LocalConfinedRoot {
    root: std::sync::Arc<LocalRoot>,
    /// The root as a `VPath`, ONLY to be able to name what `stat` returns.
    /// Never used to resolve anything.
    vpath: norte_proto::VPath,
}

impl LocalConfinedRoot {
    pub(crate) fn new(root: LocalRoot, vpath: norte_proto::VPath) -> Self {
        Self {
            root: std::sync::Arc::new(root),
            vpath,
        }
    }

    /// The `VPath` of `rel` under the root. It's for RENDERING a `stat`'s
    /// result, not for opening anything.
    fn vpath_of(&self, rel: &[Segment]) -> norte_proto::VPath {
        let mut p = self.vpath.clone();
        for seg in rel {
            p = p.join(seg.clone());
        }
        p
    }
}

#[async_trait::async_trait]
impl norte_vfs::ConfinedRoot for LocalConfinedRoot {
    async fn root_id(&self) -> Result<Option<norte_vfs::NodeId>, Error> {
        let root = std::sync::Arc::clone(&self.root);
        crate::provider::blocking(move || {
            let (dev, ino) = node_id_of(root.fd.as_raw_fd())?;
            Ok(Some(norte_vfs::NodeId {
                volume: dev,
                index: u128::from(ino),
            }))
        })
        .await
    }

    async fn mkdir(&self, rel: &[Segment]) -> Result<(), Error> {
        let root = std::sync::Arc::clone(&self.root);
        let rel = rel.to_vec();
        crate::provider::blocking(move || root.mkdir(&rel)).await
    }

    async fn write(&self, rel: &[Segment]) -> Result<Box<dyn norte_vfs::ByteSink>, Error> {
        let root = std::sync::Arc::clone(&self.root);
        let rel = rel.to_vec();
        let staging = crate::provider::blocking(move || root.open_write(&rel)).await?;
        Ok(Box::new(ConfinedSink::new(staging)))
    }

    async fn symlink(
        &self,
        rel: &[Segment],
        target: &[u8],
        _kind: norte_vfs::SymlinkKind,
    ) -> Result<(), Error> {
        // `kind` is a Windows thing, and on Windows this module doesn't exist.
        let root = std::sync::Arc::clone(&self.root);
        let rel = rel.to_vec();
        let target = target.to_vec();
        crate::provider::blocking(move || root.symlink(&rel, &target)).await
    }

    fn resumes(&self) -> bool {
        true
    }

    async fn open_resumable(
        &self,
        rel: &[Segment],
    ) -> Result<(Box<dyn norte_vfs::ByteSink>, u64), Error> {
        let root = std::sync::Arc::clone(&self.root);
        let rel = rel.to_vec();
        let (staging, already) =
            crate::provider::blocking(move || root.open_resumable(&rel)).await?;
        Ok((
            Box::new(ConfinedSink::from_existing(staging, already)),
            already,
        ))
    }

    async fn stat(&self, rel: &[Segment]) -> Result<Entry, Error> {
        let root = std::sync::Arc::clone(&self.root);
        let path = self.vpath_of(rel);
        let rel = rel.to_vec();
        crate::provider::blocking(move || root.stat(&rel, path)).await
    }

    async fn node_id(&self, rel: &[Segment]) -> Result<Option<norte_vfs::NodeId>, Error> {
        let root = std::sync::Arc::clone(&self.root);
        let rel = rel.to_vec();
        crate::provider::blocking(move || root.node_id(&rel).map(Some)).await
    }

    async fn remove(&self, rel: &[Segment]) -> Result<(), Error> {
        let root = std::sync::Arc::clone(&self.root);
        let rel = rel.to_vec();
        crate::provider::blocking(move || root.remove(&rel)).await
    }

    async fn rmdir(&self, rel: &[Segment]) -> Result<(), Error> {
        let root = std::sync::Arc::clone(&self.root);
        let rel = rel.to_vec();
        crate::provider::blocking(move || root.rmdir(&rel)).await
    }

    async fn partial_digest(&self, rel: &[Segment], len: u64) -> Result<Option<[u8; 32]>, Error> {
        let root = std::sync::Arc::clone(&self.root);
        let rel = rel.to_vec();
        crate::provider::blocking(move || root.partial_digest(&rel, len)).await
    }
}

/// The sink of a confined write.
///
/// It holds the directory's descriptor, not its path: between `write` and
/// `commit` nobody can replace a component with a symlink and divert the
/// publication, because there's no path left to resolve again.
#[derive(Debug)]
struct ConfinedSink {
    dir: Option<OwnedFd>,
    file: Option<std::fs::File>,
    /// Bytes already delivered, holes included: the anchor for
    /// [`crate::provider::write_maybe_sparse`]. With `open_write` it
    /// starts at zero; **resuming it starts at what was already there**,
    /// and not at whatever the descriptor says — the staging is opened
    /// with `O_APPEND`, which leaves the offset at 0 until the first
    /// write. That's exactly the mistake the sink next door made.
    pos: u64,
    staging: CString,
    final_name: CString,
    /// The staging carries the STABLE name: `keep` PRESERVES it so a later
    /// resume can continue it (#297).
    stable: bool,
    /// `true` once commit/abort have already dealt with the staging (Drop
    /// touches nothing).
    done: bool,
}

impl ConfinedSink {
    fn new(s: ConfinedStaging) -> Self {
        Self::from_existing(s, 0)
    }

    /// Resuming: the position starts at what the staging already had.
    fn from_existing(s: ConfinedStaging, already: u64) -> Self {
        Self {
            dir: Some(s.dir),
            file: Some(s.file),
            pos: already,
            staging: s.staging,
            final_name: s.final_name,
            stable: s.estable,
            done: false,
        }
    }
}

#[async_trait::async_trait]
impl norte_vfs::ByteSink for ConfinedSink {
    async fn write(&mut self, chunk: bytes::Bytes) -> Result<(), Error> {
        let file = self.file.take().ok_or(Error::Io { retryable: false })?;
        let mut pos = self.pos;
        let (file, pos, res) = tokio::task::spawn_blocking(move || {
            let mut file = file;
            let res = crate::provider::write_maybe_sparse(&mut file, &mut pos, &chunk)
                .map_err(|e| crate::provider::map_io(&e));
            (file, pos, res)
        })
        .await
        .map_err(|_| Error::Internal { panic: true })?;
        self.file = Some(file);
        self.pos = pos;
        res
    }

    async fn commit(mut self: Box<Self>) -> Result<(), Error> {
        let file = self.file.take().ok_or(Error::Io { retryable: false })?;
        let dir = self.dir.take().ok_or(Error::Io { retryable: false })?;
        let staging = self.staging.clone();
        let final_name = self.final_name.clone();
        let stable = self.stable;
        let res = crate::provider::blocking(move || {
            file.sync_all().map_err(|e| crate::provider::map_io(&e))?;
            // The descriptor stays alive during `publish` on purpose
            // (#299): the mode is restored AFTER publishing and on the fd.
            // Relaxing it earlier would leave readable by others a staging
            // file with the directory's most predictable name.
            let out = publish(dir.as_raw_fd(), &staging, &final_name);
            if out.is_err() {
                // A publish that doesn't publish doesn't leave the staging
                // lying around: it's the same promise the usual sink
                // makes.
                let _ = discard(dir.as_raw_fd(), &staging);
            } else {
                crate::provider::reponer_modo_publicado(&file, stable);
            }
            drop(file);
            out
        })
        .await;
        if !matches!(res, Err(Error::Internal { .. })) {
            self.done = true;
        }
        res
    }

    async fn abort(mut self: Box<Self>) -> Result<(), Error> {
        self.file.take();
        let dir = self.dir.take().ok_or(Error::Io { retryable: false })?;
        self.done = true;
        let staging = self.staging.clone();
        crate::provider::blocking(move || discard(dir.as_raw_fd(), &staging)).await
    }

    /// **With an EPHEMERAL staging, `keep` deletes, and that's not a
    /// contradiction with the trait: it's the only honest way to fulfill
    /// it.**
    ///
    /// `keep` exists to preserve the staging so a later `open_resumable`
    /// can continue it (ADR 0012). An ephemeral name — pid and sequence —
    /// can't be found again by anyone: keeping it would leave a
    /// `.norte-partial` per attempt that no resume is ever going to
    /// consume and that the next `Mirror` would see as an orphan and
    /// delete. With no resume to serve, keeping preserves nothing; it only
    /// litters.
    ///
    /// With the STABLE staging (#297) `keep` does preserve, which is what
    /// that name exists to allow: `open_resumable` finds it again and
    /// continues after its bytes.
    async fn keep(mut self: Box<Self>) -> Result<(), Error> {
        if !self.stable {
            return self.abort().await;
        }
        // Syncs what was written and lets go: the staging stays where it
        // is, with its rediscoverable name. Without `fsync`, what's kept
        // could be less than what resume is going to accept as good.
        let file = self.file.take().ok_or(Error::Io { retryable: false })?;
        self.dir.take();
        self.done = true;
        crate::provider::blocking(move || file.sync_all().map_err(|e| crate::provider::map_io(&e)))
            .await
    }
}

impl Drop for ConfinedSink {
    fn drop(&mut self) {
        // A sink dropped without commit or abort leaves no staging behind.
        // Best-effort and synchronous on purpose: there's nobody left here
        // to hand an error to.
        if self.done {
            return;
        }
        self.file.take();
        // A STABLE staging survives the drop: it's what a later resume is
        // going to look for, and deleting it here would turn a crashed
        // process into a file that has to be copied over entirely again.
        if self.stable {
            self.dir.take();
            return;
        }
        if let Some(dir) = self.dir.take() {
            let _ = discard(dir.as_raw_fd(), &self.staging);
        }
    }
}
