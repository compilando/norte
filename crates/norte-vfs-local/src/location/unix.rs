//! [`super::ConfinedRoot`]'s primitives on unix: a [`LocalRoot`] (the
//! `openat2(RESOLVE_BENEATH)` of #164) and `*at` calls on its descriptors.

use std::ffi::CString;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::Path;

use norte_proto::Segment;
use norte_vfs::NodeId;

use super::{LocationDirent, LocationError, LocationKind, LocationMeta};
use crate::confined::LocalRoot;

/// A directory resolved under the root.
pub(super) type Dir = OwnedFd;

/// The opened root.
#[derive(Debug)]
pub(super) struct Root {
    root: LocalRoot,
    id: NodeId,
}

impl Root {
    pub(super) fn open(dir: &Path) -> Result<Self, LocationError> {
        let root = LocalRoot::open(dir).map_err(|e| from_proto(&e))?;
        let id = fd_id(root.raw_fd())?;
        Ok(Self { root, id })
    }

    pub(super) const fn id(&self) -> NodeId {
        self.id
    }

    pub(super) fn dir(&self, comps: &[Segment]) -> Result<Dir, LocationError> {
        self.root.resolve_dir(comps).map_err(|e| from_proto(&e))
    }
}

pub(super) fn dir_id(dir: &Dir) -> Result<NodeId, LocationError> {
    fd_id(dir.as_raw_fd())
}

fn fd_id(fd: RawFd) -> Result<NodeId, LocationError> {
    let (dev, ino) = crate::confined::node_id_of(fd).map_err(|e| from_proto(&e))?;
    Ok(NodeId {
        volume: dev,
        index: u128::from(ino),
    })
}

fn c_name(name: &Segment) -> Result<CString, LocationError> {
    CString::new(name.as_bytes().to_vec()).map_err(|_| LocationError::Io)
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
pub(super) fn open_read(dir: &Dir, name: &Segment) -> Result<std::fs::File, LocationError> {
    let name = c_name(name)?;
    #[allow(unsafe_code)]
    // SAFETY: `dir` is alive (the caller's `OwnedFd` holds it) and `name`
    // is a NUL-terminated CString alive for the whole call. The fd the
    // kernel returns has no owner until the `from_raw_fd` below.
    let raw = unsafe {
        libc::openat(
            dir.as_raw_fd(),
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

/// `fstatat` without following the final symlink; `None` is the directory
/// itself (a dirfd's `.`).
pub(super) fn stat(dir: &Dir, name: Option<&Segment>) -> Result<LocationMeta, LocationError> {
    let name = match name {
        Some(n) => c_name(n)?,
        None => c".".to_owned(),
    };
    let mut st: libc::stat = unsafe_zeroed_stat();
    #[allow(unsafe_code)]
    // SAFETY: `dir` is alive, `name` is NUL-terminated and alive, and `st`
    // is an owned, aligned `stat` the kernel fills in entirely.
    let rc = unsafe {
        libc::fstatat(
            dir.as_raw_fd(),
            name.as_ptr(),
            &raw mut st,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
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
pub(super) fn list(dir: &Dir, max: u32) -> Result<Vec<LocationDirent>, LocationError> {
    // `fdopendir` takes ownership of the fd (`closedir` closes it), so it's
    // given a duplicate OPENED FOR READING: `LocalRoot`'s is `O_PATH`,
    // which doesn't work for iterating.
    #[allow(unsafe_code)]
    // SAFETY: `dir` is alive for the call; `"."` is a NUL-terminated
    // constant. The returned fd has no owner until the `fdopendir`.
    let raw = unsafe {
        libc::openat(
            dir.as_raw_fd(),
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

pub(super) fn from_io(e: &std::io::Error) -> LocationError {
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
