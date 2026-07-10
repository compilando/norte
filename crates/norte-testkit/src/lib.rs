//! Kit de testing de norte: `MemProvider` determinista con inyección de fallos,
//! corpus canónico de fixtures hostiles (nombres y contenidos) y estrategias
//! proptest compartidas (spec §12).
//!
//! Regla del proyecto: todo bug de encoding/paths añade su fixture aquí ANTES
//! del fix.
#![forbid(unsafe_code)]

pub mod corpus;
mod faults;
mod mem;
pub mod strategies;

pub use faults::Faults;
pub use mem::{MemProvider, Normalization};
