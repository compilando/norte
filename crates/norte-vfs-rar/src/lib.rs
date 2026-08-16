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

pub use delegate::{Delegate, RarError};
