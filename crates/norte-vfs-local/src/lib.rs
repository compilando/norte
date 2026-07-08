//! Provider VFS del filesystem local, por OS (`cfg(unix)` / `cfg(windows)`).
//!
//! Único crate del workspace autorizado a usar `unsafe` (regla 5 de `CLAUDE.md`):
//! reconstrucción de `OsString` desde bytes y syscalls específicas de OS.
//! Cada uso se habilita por ítem con `#[allow(unsafe_code)]`, lleva comentario
//! `// SAFETY:` y test que ejercita la invariante.
#![deny(unsafe_code)]
