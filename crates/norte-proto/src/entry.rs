//! `Entry`: lo que el VFS sabe de un nodo (resultado de `fs.stat` / `fs.list`).

use serde::{Deserialize, Serialize};

use crate::VPath;

/// Clase de un nodo del VFS.
///
/// Los valores desconocidos (protocolo N+1) deserializan a [`EntryKind::Other`]:
/// un cliente viejo degrada, no revienta.
///
/// ```
/// use norte_proto::EntryKind;
/// let k: EntryKind = serde_json::from_str("\"kind_del_futuro\"").unwrap();
/// assert_eq!(k, EntryKind::Other);
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    /// Archivo regular.
    File,
    /// Directorio.
    Dir,
    /// Symlink (M0: jamás se sigue; se preserva o `Unsupported`).
    Symlink,
    /// Cualquier otra cosa (device, socket, fifo, o kind de protocolo futuro).
    #[serde(other)]
    Other,
}

/// Metadatos de un nodo del VFS.
///
/// Campos opcionales `None` = "el provider no lo sabe", nunca un 0 fingido.
///
/// ```
/// use norte_proto::attrs::AttrValue;
/// use norte_proto::{Entry, EntryKind, VPath};
/// let e = Entry {
///     path: VPath::parse("file:///a.txt").unwrap(),
///     kind: EntryKind::File,
///     size: Some(42),
///     mtime_ms: None,
///     attrs: [("posix.mode".to_owned(), AttrValue::Uint(0o100_644))].into(),
/// };
/// let json = serde_json::to_string(&e).unwrap();
/// assert_eq!(serde_json::from_str::<Entry>(&json).unwrap(), e);
///
/// // Sin attrs, el wire es EXACTAMENTE el de 0.29.
/// let plain = Entry { attrs: Default::default(), ..e };
/// assert!(!serde_json::to_string(&plain).unwrap().contains("attrs"));
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Entry {
    /// Path completo del nodo (los frontends operan con él, no con el nombre).
    pub path: VPath,
    /// Clase del nodo.
    pub kind: EntryKind,
    /// Tamaño en bytes; `None` si el provider no lo conoce (p. ej. directorios).
    #[serde(default)]
    pub size: Option<u64>,
    /// Última modificación en milisegundos desde epoch UTC; `i64` porque las
    /// fechas pre-1970 existen en FS reales. `None` = desconocida.
    #[serde(default)]
    pub mtime_ms: Option<i64>,
    /// Provider attributes (0.30.0, ADR 0039): typed metadata BEYOND the four
    /// fields above, delivered only for the ids the client requested in
    /// `fs.list`/`fs.stat`. An absent key means the provider does not know the
    /// value — never a fabricated one. Empty (the default, and the only
    /// possibility for a 0.29 peer) is omitted from the wire entirely, so a
    /// payload without attributes is byte-identical to 0.29's.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub attrs: std::collections::BTreeMap<String, crate::attrs::AttrValue>,
}
