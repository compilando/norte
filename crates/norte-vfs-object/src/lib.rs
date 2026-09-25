//! VFS provider over object storage via opendal (ADR 0016): S3 first,
//! GCS/Azure later by turning on workspace features without touching this
//! code.
//!
//! The [`opendal::Operator`] arrives INJECTED already configured (bucket,
//! region, endpoint, credentials) from `norte-connect` (phase 7d): this
//! crate never sees a secret (CLAUDE.md rules 7/10).

#![forbid(unsafe_code)]

mod provider;

pub use provider::ObjectProvider;

// The only opendal type that crosses the public boundary is the
// constructor's (norte-vfs-ftp's `FtpStream` pattern): norte-connect builds
// it and the core does not depend on opendal directly.
pub use opendal::Operator;
