//! Provider VFS read-only de archivos RAR, **delegando** en un `7z` o `unrar`
//! ya instalado (decisión de producto 5 del roadmap post-alpha).
//!
//! El descompresor de RAR es no libre: no hay forma de leer un `.rar`
//! comprimido con código que este árbol pueda contener. La salida es delegar
//! en un programa externo — y entonces el problema deja de ser el formato y
//! pasa a ser la **regla 9**: al delegado se le da una ruta y una tubería,
//! jamás el sistema de ficheros del usuario.
//!
//! Por eso este crate NO compone sobre otro
//! [`Provider`](norte_vfs::Provider) como hace `norte-vfs-archive`: sostiene
//! una **ruta local** al archivo y nada más, así que no puede alcanzar un byte
//! remoto ni conocer a otros providers. Quién puede montar un `rar` — solo
//! sobre `file://` — lo decide el dispatch del engine, no este crate.
#![forbid(unsafe_code)]

mod delegate;
mod index;
mod listing;
mod provider;

pub use delegate::{Delegate, LIST_TIMEOUT, RarError};
pub use index::ArchiveIndex;
pub use listing::{Listing, RawEntry, parse_7z_slt, parse_unrar_vt};
pub use provider::RarProvider;

/// Topes anti-bomba del índice de un `.rar`, hermanos de los de ADR 0018.
///
/// Superarlos NO declara roto el archivo: omiten la entrada, la cuentan y
/// siguen. Un `.rar` con un nombre absurdo se explora igual, con una entrada
/// menos y el contador diciéndolo.
///
/// ```
/// let flojos = norte_vfs_rar::RarLimits { max_entries: 10, ..Default::default() };
/// assert_eq!(flojos.max_depth, norte_vfs_rar::RarLimits::default().max_depth);
/// ```
#[derive(Debug, Clone, Copy)]
pub struct RarLimits {
    /// Tope de entradas indexadas.
    pub max_entries: usize,
    /// Tope de bytes del nombre completo de una entrada.
    pub max_name_bytes: usize,
    /// Tope de componentes de path de una entrada.
    pub max_depth: usize,
}

impl Default for RarLimits {
    fn default() -> Self {
        Self {
            max_entries: 500_000,
            max_name_bytes: 4_096,
            max_depth: 64,
        }
    }
}
