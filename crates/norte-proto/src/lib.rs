//! Tipos del protocolo `norte`: el wire format JSON-RPC, sin lógica de negocio.
//!
//! Cualquier cambio en este crate es un cambio de wire format: exige golden
//! test actualizado, bump de versión de protocolo y revisión doble
//! (regla dura de `CLAUDE.md`; procedimiento en la spec §11).
#![forbid(unsafe_code)]

mod wire;

pub mod vpath;

pub use vpath::{Authority, Scheme, Segment, VPath, VPathError};
