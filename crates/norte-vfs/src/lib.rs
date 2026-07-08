//! Contrato central del VFS de norte: el trait `Provider` y sus tipos.
//!
//! Todo backend de almacenamiento (local, sftp, s3, archive, memoria) implementa
//! este trait y pasa la misma suite contractual (`provider_contract!`).
//! Los providers no se conocen entre sí; las operaciones compuestas viven en
//! `norte-core` (spec §5).
#![forbid(unsafe_code)]

pub use norte_proto as proto;
