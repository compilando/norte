//! VFS provider for the local filesystem, per OS (`cfg(unix)` / `cfg(windows)`).
//!
//! The only crate in the workspace AUTHORIZED to use `unsafe` (rule 5 of
//! `CLAUDE.md`), with `#[allow(unsafe_code)]` per item and `// SAFETY:` on
//! every block. Uses: `rename_noreplace` (syscalls std doesn't expose —
//! `renameat2`/`renamex_np`/`MoveFileExW` without replace); `mounts_macos`/
//! `mounts_windows` (2026-08-10-volumes.md task V4), the raw
//! `getmntinfo`/`GetVolumeInformationW` and friends FFI that
//! `norte-core::volumes` needs but can't touch directly; `trash_fdo`
//! (`getuid` and `localtime_r`); `kernel_copy` (`FICLONE` and
//! `copy_file_range`, ADR 0165). Rebuilding
//! `OsString` on Windows stays 100% safe: WTF-8 validated → UTF-16 →
//! `from_wide` (the unchecked variant stays forbidden).
#![deny(unsafe_code)]

mod caps_at;
#[cfg(unix)]
mod confined;
/// Writes that cannot escape their root, on Windows (#217, ADR 0160).
#[cfg(windows)]
mod confined_windows;
#[cfg(unix)]
mod identity;
/// Reflink and `copy_file_range` for local copies (ADR 0165).
#[cfg(unix)]
mod kernel_copy;
/// Bounded reading under a directory, for the plugin-host's `location`
/// capability (ADR 0057, ADR 0158).
#[cfg(any(unix, windows))]
mod location;
#[cfg(target_os = "macos")]
pub mod mounts_macos;
#[cfg(windows)]
pub mod mounts_windows;
/// Names opened, renamed and deleted relative to a directory handle.
#[cfg(windows)]
mod win_nt;

mod provider;
/// Built everywhere so its tests run on every gate; only Windows calls it.
#[cfg_attr(not(windows), allow(dead_code))]
mod shell_name;
/// Our own freedesktop trash (Linux/BSD): the only one that knows WHERE it
/// left the file, which is what undo needs.
#[cfg(all(
    unix,
    not(target_os = "macos"),
    not(target_os = "ios"),
    not(target_os = "android")
))]
mod trash_fdo;
/// The Recycle Bin, refusing what the shell would destroy (#25).
#[cfg(windows)]
mod trash_windows;

#[cfg(any(unix, windows))]
pub use location::{
    Bounds, ConfinedRoot, LocationDirent, LocationError, LocationKind, LocationMeta,
};
// The two conversions LIVE in `norte-vfs` since #254: they're shape rules
// and two frontends that don't want a provider in-process need them.
// Re-exported here because this used to be their home and the core calls
// them this way.
pub use norte_vfs::native::{vpath_from_native, vpath_to_native};
pub use provider::LocalProvider;

/// The identity of an open file, or `None` where the volume gives none.
///
/// With [`identity_at`], it answers "does this path still name the file I
/// opened?" on every platform, which `std` only answers on unix.
///
/// ```
/// let dir = tempfile::tempdir().unwrap();
/// let path = dir.path().join("f");
/// let file = std::fs::File::create(&path).unwrap();
/// assert_eq!(
///     norte_vfs_local::identity_of(&file),
///     norte_vfs_local::identity_at(&path)
/// );
/// ```
#[must_use]
pub fn identity_of(file: &std::fs::File) -> Option<norte_vfs::NodeId> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let md = file.metadata().ok()?;
        Some(norte_vfs::NodeId {
            volume: md.dev(),
            index: u128::from(md.ino()),
        })
    }
    #[cfg(windows)]
    {
        win_nt::file_id(file).ok().filter(|id| id.index != 0)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = file;
        None
    }
}

/// The identity of what `path` names, without following a final link, or
/// `None` if it is gone or the volume gives none. See [`identity_of`].
#[must_use]
pub fn identity_at(path: &std::path::Path) -> Option<norte_vfs::NodeId> {
    provider::node_id_native(path, norte_vfs::FollowLinks::No)
        .ok()
        .flatten()
}
