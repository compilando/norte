//! Contrato central del VFS de norte: el trait `Provider` y sus tipos.
//!
//! Todo backend de almacenamiento (local, sftp, s3, archive, memoria) implementa
//! este trait y pasa la misma suite contractual (`provider_contract!`, o
//! `readonly_provider_contract!` si declara `READ_ONLY` — ADR 0018).
//! Los providers no se conocen entre sí; las operaciones compuestas viven en
//! `norte-core` (spec §5).
#![forbid(unsafe_code)]

mod contract;
mod contract_ro;
mod contract_trash;
mod provider;
mod sink;
mod trash;

pub use norte_proto as proto;
pub use norte_proto::{ByteRange, Capabilities, CapabilityFlags, Entry, EntryKind, Error, VPath};
pub use provider::{ByteStream, EntryStream, FollowLinks, NodeId, Provider, SymlinkKind};
pub use sink::ByteSink;
pub use trash::{logical_trash, now_ms};

/// Re-exports internos para la expansión de [`provider_contract!`].
/// NO es API: puede cambiar sin aviso.
#[doc(hidden)]
pub mod __private {
    pub use bytes;
    pub use futures;
    pub use norte_proto;
}
