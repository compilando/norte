//! Provider VFS del filesystem local, por OS (`cfg(unix)` / `cfg(windows)`).
//!
//! Único crate del workspace AUTORIZADO a usar `unsafe` (regla 5 de
//! `CLAUDE.md`), con `#[allow(unsafe_code)]` por ítem y `// SAFETY:` en cada
//! bloque. Usos: `rename_noreplace` (syscalls que std no expone —
//! `renameat2`/`renamex_np`/`MoveFileExW` sin replace); `mounts_macos`/
//! `mounts_windows` (2026-08-10-volumes.md tarea V4), la FFI cruda de
//! `getmntinfo`/`GetVolumeInformationW` y compañía que `norte-core::volumes`
//! necesita pero no puede tocar directamente. La reconstrucción de
//! `OsString` en Windows sigue 100% safe: WTF-8 validado → UTF-16 →
//! `from_wide` (la unchecked queda prohibida).
#![deny(unsafe_code)]

#[cfg(target_os = "macos")]
pub mod mounts_macos;
#[cfg(windows)]
pub mod mounts_windows;
mod native_path;
mod provider;

pub use native_path::{vpath_from_native, vpath_to_native};
pub use provider::LocalProvider;
