//! Provider VFS read-only de archivos comprimidos: zip/tar como directorios
//! virtuales (ADR 0018).
//!
//! No es un backend propio: COMPONE sobre otro [`Provider`](norte_vfs::Provider)
//! (local, sftp, s3, memoria) que le da los bytes del contenedor. El
//! direccionamiento es el de ADR 0018: scheme compuesto
//! `<formato>+<scheme-interior>` + segmento marcador `!`
//! (`zip+file:///a.zip/!/docs/x.txt`), con
//! [`VPath::archive_split`](norte_proto::VPath::archive_split) como única
//! fuente de verdad del parseo.
//!
//! ## Semántica (checklist de provider)
//!
//! - **Encoding de nombres**: bytes crudos, jamás decodificados (regla 1; el
//!   bit 11 de zip es solo metadato). Entradas cuyo nombre no mapea a
//!   segmentos `VPath` (`..`, `.`, vacío, NUL, absoluto, componente `!`) se
//!   OMITEN del árbol con `tracing::warn!` — contrato documentado en
//!   [`Provider::list`](norte_vfs::Provider::list) de este provider.
//! - **Symlinks**: los de tar se listan como `Symlink`; `read_link` da el
//!   target crudo; `read` sobre ellos es `TypeMismatch` (semántica lstat).
//! - **Case**: sensitive y preserving (comparación por bytes, como el
//!   contenido del archivo).
//! - **Rename atómico / trash / paths máximos**: no aplica — `READ_ONLY`;
//!   toda mutación responde [`Error::Unsupported`](norte_proto::Error).
//! - **Anti-bomba**: [`Limits`] acota entradas/nombre/profundidad del
//!   índice; superarlos es `Io { retryable: false }`.
//! - **Caché**: índice por archivo (LRU cap 8), invalidado por
//!   `(mtime_ms, size)` del contenedor; `mtime` desconocido = siempre stale.
#![forbid(unsafe_code)]

mod blocking;
mod index;
mod provider;
mod tar_format;
mod zip_format;

pub use index::Limits;
pub use provider::{ArchiveProvider, Format};
