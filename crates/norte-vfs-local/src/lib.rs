//! Provider VFS del filesystem local, por OS (`cfg(unix)` / `cfg(windows)`).
//!
//! Único crate del workspace AUTORIZADO a usar `unsafe` (regla 5 de
//! `CLAUDE.md`), con `#[allow(unsafe_code)]` por ítem y `// SAFETY:` en cada
//! bloque. Usos: `rename_noreplace` (syscalls que std no expone —
//! `renameat2`/`renamex_np`/`MoveFileExW` sin replace); `mounts_macos`/
//! `mounts_windows` (2026-08-10-volumes.md tarea V4), la FFI cruda de
//! `getmntinfo`/`GetVolumeInformationW` y compañía que `norte-core::volumes`
//! necesita pero no puede tocar directamente; `trash_fdo` (`getuid` y
//! `localtime_r`). La reconstrucción de
//! `OsString` en Windows sigue 100% safe: WTF-8 validado → UTF-16 →
//! `from_wide` (la unchecked queda prohibida).
#![deny(unsafe_code)]

mod caps_at;
#[cfg(unix)]
mod confined;
#[cfg(unix)]
mod identidad;
/// Lectura acotada bajo un directorio, para la capacidad `location` del
/// plugin-host (ADR 0057).
#[cfg(unix)]
mod location;
#[cfg(target_os = "macos")]
pub mod mounts_macos;
#[cfg(windows)]
pub mod mounts_windows;

mod provider;
/// Papelera freedesktop propia (Linux/BSD): la única que sabe DÓNDE dejó el
/// fichero, que es lo que el undo necesita.
#[cfg(all(
    unix,
    not(target_os = "macos"),
    not(target_os = "ios"),
    not(target_os = "android")
))]
mod trash_fdo;

#[cfg(unix)]
pub use location::{
    Bounds, ConfinedRoot, LocationDirent, LocationError, LocationKind, LocationMeta,
};
// Las dos conversiones VIVEN en `norte-vfs` desde #254: son reglas de forma
// y las necesitan dos frontends que no quieren un provider en el proceso.
// Se re-exportan aquí porque este era su sitio y el core las llama así.
pub use norte_vfs::native::{vpath_from_native, vpath_to_native};
pub use provider::LocalProvider;
