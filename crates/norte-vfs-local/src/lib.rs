//! Provider VFS del filesystem local, por OS (`cfg(unix)` / `cfg(windows)`).
//!
//! Único crate del workspace AUTORIZADO a usar `unsafe` (regla 5 de
//! `CLAUDE.md`)… y actualmente con CERO usos: la reconstrucción de `OsString`
//! en Windows va por WTF-8 validado → UTF-16 → `from_wide` (100% safe). Si
//! algún día hace falta, cada uso irá con `#[allow(unsafe_code)]` por ítem,
//! `// SAFETY:` y test.
#![deny(unsafe_code)]

mod native_path;
mod provider;

pub use native_path::vpath_from_native;
pub use provider::LocalProvider;
