//! Provider VFS del filesystem local, por OS (`cfg(unix)` / `cfg(windows)`).
//!
//! Único crate del workspace AUTORIZADO a usar `unsafe` (regla 5 de
//! `CLAUDE.md`). Único uso hoy: `rename_noreplace` (syscalls que std no
//! expone — `renameat2`/`renamex_np`/`MoveFileExW` sin replace), con
//! `#[allow(unsafe_code)]` por ítem, `// SAFETY:` y test. La reconstrucción
//! de `OsString` en Windows sigue 100% safe: WTF-8 validado → UTF-16 →
//! `from_wide` (la unchecked queda prohibida).
#![deny(unsafe_code)]

mod native_path;
mod provider;

pub use native_path::{vpath_from_native, vpath_to_native};
pub use provider::LocalProvider;
