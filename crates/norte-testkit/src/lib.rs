//! norte's testing kit: a deterministic `MemProvider` with fault injection,
//! the canonical corpus of hostile fixtures (names and contents) and shared
//! proptest strategies (spec §12).
//!
//! Project rule: every encoding/paths bug adds its fixture here BEFORE the
//! fix.
#![forbid(unsafe_code)]

pub mod corpus;
mod faults;
mod mem;
mod smith;
pub mod strategies;

pub use faults::Faults;
pub use mem::{MemProvider, Normalization};
pub use smith::{RarSmith, TarSmith, ZipSmith, which_7z};
