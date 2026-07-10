//! Tipos de transferencia (ADR 0005, spec §5): rango de lectura y políticas
//! del copy engine. Viajan en los params de `fs.copy`/`fs.move` y en la API
//! del trait `Provider`.

use serde::{Deserialize, Serialize};

/// Rango de bytes de una lectura: `offset` inicial y longitud opcional
/// (`None` = hasta EOF). Lo exigen el resume de M2 (`.norte-partial` +
/// offset) y el viewer (lectura parcial de archivos grandes).
///
/// ```
/// use norte_proto::ByteRange;
/// let r: ByteRange = serde_json::from_str(r#"{"offset": 65536, "len": null}"#).unwrap();
/// assert_eq!(r, ByteRange { offset: 65536, len: None });
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ByteRange {
    /// Primer byte a leer (0 = principio).
    pub offset: u64,
    /// Cuántos bytes leer; `None` = hasta el final del archivo.
    pub len: Option<u64>,
}

/// Qué hacer cuando el destino de una copia/movimiento ya existe
/// (spec §5; semánticas exactas en ADR 0005).
///
/// El engine de M1 trata [`Ask`](Self::Ask) como [`Fail`](Self::Fail): la
/// resolución interactiva por archivo llega con los diálogos del TUI
/// (fase 5). Un core viejo que no conozca una política nueva DEBE fallar el
/// request, jamás adivinar.
///
/// ```
/// use norte_proto::CollisionPolicy;
/// let p: CollisionPolicy = serde_json::from_str(r#""rename_auto""#).unwrap();
/// assert_eq!(p, CollisionPolicy::RenameAuto);
/// assert_eq!(CollisionPolicy::default(), CollisionPolicy::Fail);
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CollisionPolicy {
    /// La colisión es error: `Conflict` y la task falla (default).
    #[default]
    Fail,
    /// Preguntar por archivo (TUI, fase 5). El engine M1 lo trata como
    /// [`Fail`](Self::Fail).
    Ask,
    /// No copiar la entrada en conflicto; la task termina `Completed`.
    Skip,
    /// Reemplazar el destino (remove + write, dos mutaciones en el journal;
    /// tipo contra tipo distinto sigue siendo `Conflict`).
    Overwrite,
    /// Buscar nombre libre: sufijo ` (n)` antes de la última extensión,
    /// n = 1..=1000; agotado → `Conflict`.
    RenameAuto,
    /// Reemplazar solo si el origen es más nuevo (`mtime`); si no, saltar.
    /// Sin mtime comparable en cualquiera de los dos → `Conflict`.
    Newer,
}

/// Qué hacer con los symlinks al copiar (spec §17.9; ADR 0005).
///
/// En M1, [`Follow`](Self::Follow) sobre un symlink a DIRECTORIO devuelve
/// `Unsupported` (seguir dirs exige detección de ciclos — M2).
///
/// ```
/// use norte_proto::SymlinkPolicy;
/// let p: SymlinkPolicy = serde_json::from_str(r#""preserve""#).unwrap();
/// assert_eq!(p, SymlinkPolicy::Preserve);
/// assert_eq!(SymlinkPolicy::default(), SymlinkPolicy::Preserve);
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymlinkPolicy {
    /// Copiar el CONTENIDO apuntado. Symlink a dir: `Unsupported` en M1.
    Follow,
    /// Recrear el symlink en el destino, bytes del target intactos
    /// (default: lo que hace `cp -a`).
    #[default]
    Preserve,
    /// No copiar symlinks (contados como saltados).
    Skip,
}
