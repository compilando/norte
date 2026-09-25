//! VFS provider over SFTP/SSH (ADR 0013), with `russh` + `russh-sftp`.
//!
//! The [`SftpProvider`] wraps an ALREADY established `SftpSession` (session
//! injection): the SSH connection with auth and host key verification lands
//! in phase 6 (`connections.toml` + keyring). What lives here is the
//! provider's logic and its containment of a hostile server (`../../`, trap
//! symlinks) — see [`SftpProvider`].
//!
//! The `russh-sftp` types do NOT cross the public boundary: outside the
//! crate only the [`norte_vfs::Provider`] trait is visible.
#![forbid(unsafe_code)]

mod provider;

pub use provider::SftpProvider;
