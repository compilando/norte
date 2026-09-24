//! Read-only VFS provider for compressed archives: zip/tar as virtual
//! directories (ADR 0018).
//!
//! It isn't a backend of its own: it COMPOSES over another
//! [`Provider`](norte_vfs::Provider) (local, sftp, s3, memory) that gives
//! it the container's bytes. Addressing follows ADR 0018: a composite
//! scheme `<format>+<inner-scheme>` + `!` marker segment
//! (`zip+file:///a.zip/!/docs/x.txt`), with
//! [`VPath::archive_split`](norte_proto::VPath::archive_split) as the sole
//! source of truth for parsing.
//!
//! ## Semantics (provider checklist)
//!
//! - **Name encoding**: raw bytes, never decoded (rule 1; zip's bit 11 is
//!   only metadata). Entries whose name doesn't map to `VPath` segments
//!   (`..`, `.`, empty, NUL, absolute, a `!` component) are OMITTED from
//!   the tree with `tracing::warn!` — a contract documented on this
//!   provider's [`Provider::list`](norte_vfs::Provider::list).
//! - **Symlinks**: tar's are listed as `Symlink`; `read_link` gives the
//!   raw target; `read` over them is `TypeMismatch` (lstat semantics).
//! - **Case**: sensitive and preserving (byte comparison, like the
//!   archive's content).
//! - **Atomic rename / trash / max paths**: doesn't apply — `READ_ONLY`;
//!   every mutation answers [`Error::Unsupported`](norte_proto::Error).
//! - **Anti-bomb**: [`Limits`] bounds the index's entries/name/depth;
//!   exceeding them is `Corrupt`.
//! - **Cache**: per-archive index (LRU cap 8), invalidated by the
//!   container's `(mtime_ms, size)`; unknown `mtime` = always stale.
#![forbid(unsafe_code)]

mod blocking;
mod index;
mod provider;
mod tar_format;
mod targz_format;
pub mod write;
mod zip_cd;
mod zip_format;

pub use index::Limits;
pub use provider::{ArchiveProvider, Format};
