//! VFS provider for the local filesystem, per OS (`cfg(unix)` / `cfg(windows)`).
//!
//! The only crate in the workspace AUTHORIZED to use `unsafe` (rule 5 of
//! `CLAUDE.md`), with `#[allow(unsafe_code)]` per item and `// SAFETY:` on
//! every block. Uses: `rename_noreplace` (syscalls std doesn't expose —
//! `renameat2`/`renamex_np`/`MoveFileExW` without replace); `mounts_macos`/
//! `mounts_windows` (2026-08-10-volumes.md task V4), the raw
//! `getmntinfo`/`GetVolumeInformationW` and friends FFI that
//! `norte-core::volumes` needs but can't touch directly; `trash_fdo`
//! (`getuid` and `localtime_r`). Rebuilding
//! `OsString` on Windows stays 100% safe: WTF-8 validated → UTF-16 →
//! `from_wide` (the unchecked variant stays forbidden).
#![deny(unsafe_code)]

mod caps_at;
#[cfg(unix)]
mod confined;
#[cfg(unix)]
mod identity;
/// Bounded reading under a directory, for the plugin-host's `location`
/// capability (ADR 0057).
#[cfg(unix)]
mod location;
#[cfg(target_os = "macos")]
pub mod mounts_macos;
#[cfg(windows)]
pub mod mounts_windows;

mod provider;
/// Our own freedesktop trash (Linux/BSD): the only one that knows WHERE it
/// left the file, which is what undo needs.
#[cfg(all(
    unix,
    not(target_os = "macos"),
    not(target_os = "ios"),
    not(target_os = "android")
))]
mod trash_fdo;

#[cfg(unix)]
pub use location::{
    Bounds, ConfinedRoot, LocationDirent, LocationError, LocationKind, LocationMeta,
};
// The two conversions LIVE in `norte-vfs` since #254: they're shape rules
// and two frontends that don't want a provider in-process need them.
// Re-exported here because this used to be their home and the core calls
// them this way.
pub use norte_vfs::native::{vpath_from_native, vpath_to_native};
pub use provider::LocalProvider;
