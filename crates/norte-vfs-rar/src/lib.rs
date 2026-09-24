//! Read-only VFS provider for RAR archives, **delegating** to an already
//! installed `7z` or `unrar` (post-alpha roadmap product decision 5).
//!
//! RAR's decompressor is non-free: there is no way to read a compressed
//! `.rar` with code this tree can contain. The way out is delegating to an
//! external program — and then the problem stops being the format and
//! becomes **rule 9**: the delegate is given a path and a pipe, never the
//! user's filesystem.
//!
//! That is why this crate does NOT compose over another
//! [`Provider`](norte_vfs::Provider) the way `norte-vfs-archive` does: it
//! holds a **local path** to the archive and nothing else, so it cannot
//! reach a remote byte or know about other providers. Who gets to mount a
//! `rar` — only over `file://` — is decided by the engine's dispatch, not
//! this crate.
#![forbid(unsafe_code)]

mod delegate;
mod index;
mod listing;
mod provider;

pub use delegate::{Delegate, LIST_TIMEOUT, RarError};
pub use index::ArchiveIndex;
pub use listing::{Listing, RawEntry, parse_7z_slt, parse_unrar_vt};
pub use provider::RarProvider;

/// Anti-bomb caps for a `.rar`'s index, siblings of ADR 0018's.
///
/// Exceeding them does NOT mark the archive broken: the entry is skipped,
/// counted, and it moves on. A `.rar` with an absurd name is still
/// explored, with one fewer entry and the counter saying so.
///
/// ```
/// let loose = norte_vfs_rar::RarLimits { max_entries: 10, ..Default::default() };
/// assert_eq!(loose.max_depth, norte_vfs_rar::RarLimits::default().max_depth);
/// ```
#[derive(Debug, Clone, Copy)]
pub struct RarLimits {
    /// Cap on indexed entries.
    pub max_entries: usize,
    /// Cap on an entry's full name, in bytes.
    pub max_name_bytes: usize,
    /// Cap on an entry's path components.
    pub max_depth: usize,
}

impl Default for RarLimits {
    fn default() -> Self {
        Self {
            max_entries: 500_000,
            max_name_bytes: 4_096,
            max_depth: 64,
        }
    }
}
