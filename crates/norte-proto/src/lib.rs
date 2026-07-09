//! Tipos del protocolo `norte`: el wire format JSON-RPC, sin lógica de negocio.
//!
//! Cualquier cambio en este crate es un cambio de wire format: exige golden
//! test actualizado, bump de versión de protocolo y revisión doble
//! (regla dura de `CLAUDE.md`; procedimiento en la spec §11).
#![forbid(unsafe_code)]

mod wire;

pub mod caps;
pub mod entry;
pub mod error;
pub mod methods;
pub mod task;
pub mod vpath;

pub use caps::{Capabilities, CapabilityFlags};
pub use entry::{Entry, EntryKind};
pub use error::{ConflictKind, Error};
pub use methods::PROTOCOL_VERSION;
pub use task::{TaskId, TaskKind, TaskProgress, TaskState};
pub use vpath::{Authority, Scheme, Segment, VPath, VPathError};
