//! Taxonomía de errores del protocolo (spec §17.7): estable, documentada,
//! renderizable por categoría. Los frontends NUNCA parsean strings de error;
//! el mapeo desde errores de OS/provider ocurre en el borde (vfs-local, core).

use std::fmt;

use serde::{Deserialize, Serialize};

/// Subtipo de conflicto en el destino de una operación.
/// Tolerancia N/N-1 (patrón de ADR 0004; el fallback lo introduce ADR
/// 0005): un subtipo desconocido deserializa a [`ConflictKind::Unknown`] —
/// el cliente viejo degrada a "conflicto genérico", no revienta.
///
/// ```
/// use norte_proto::ConflictKind;
/// let futuro: ConflictKind = serde_json::from_str(r#""subtipo_del_futuro""#).unwrap();
/// assert_eq!(futuro, ConflictKind::Unknown);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ConflictKind {
    /// El destino ya existe.
    Exists,
    /// Colisión solo-por-caja en FS case-insensitive (evaluada contra el
    /// FS DESTINO, no el origen).
    CaseCollision,
    /// Colisión solo-por-normalización Unicode: los bytes difieren pero la
    /// forma NFC coincide (macOS almacena NFD; issue #8, ADR 0005).
    Normalization,
    /// El destino existe con otro tipo (dir donde va un archivo o viceversa).
    TypeMismatch,
    /// Subtipo de un protocolo más nuevo (fallback de deserialización).
    /// El core JAMÁS lo emite.
    #[doc(hidden)]
    #[serde(other)]
    Unknown,
}

impl fmt::Display for ConflictKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Exists => "destination exists",
            Self::CaseCollision => "case-insensitive collision",
            Self::Normalization => "unicode normalization collision",
            Self::TypeMismatch => "destination type mismatch",
            Self::Unknown => "unknown conflict kind (newer protocol)",
        })
    }
}

/// Error del protocolo norte (spec §17.7).
///
/// Wire: objeto tagged `{"kind": "...", …campos}`. La variante es la API:
/// los frontends hacen match por categoría y el detalle humano viaja aparte
/// (campo `message` del error JSON-RPC), nunca dentro de esta taxonomía.
///
/// Tolerancia N/N-1 (ADR 0004): una categoría desconocida deserializa a
/// [`Error::Unknown`] — el cliente viejo degrada a "error genérico", no
/// revienta. `#[non_exhaustive]` obliga además al brazo `_` en Rust.
///
/// ```
/// use norte_proto::Error;
/// let e: Error = serde_json::from_str(r#"{"kind": "not_found"}"#).unwrap();
/// assert_eq!(e, Error::NotFound);
/// let futuro: Error = serde_json::from_str(r#"{"kind": "quota_del_futuro"}"#).unwrap();
/// assert_eq!(futuro, Error::Unknown);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Error {
    /// El path no existe.
    #[error("not found")]
    NotFound,
    /// El provider/OS denegó el acceso.
    #[error("permission denied")]
    PermissionDenied,
    /// Conflicto en el destino; la operación NO escribió nada.
    #[error("conflict: {conflict}")]
    Conflict {
        /// Subtipo del conflicto.
        conflict: ConflictKind,
    },
    /// El provider no responde (red caída, daemon remoto muerto…).
    #[error("provider unavailable (retryable: {retryable})")]
    ProviderUnavailable {
        /// `true` si reintentar con backoff tiene sentido.
        retryable: bool,
    },
    /// Sin espacio o cuota en el destino (ENOSPC/EDQUOT) — el fallo de copia
    /// más común tras permisos; merece render propio, no un cajón genérico.
    #[error("no space left on destination")]
    NoSpace,
    /// I/O falló a mitad de operación (EIO, reset de conexión…): el provider
    /// responde, pero esta operación concreta murió.
    #[error("i/o error (retryable: {retryable})")]
    Io {
        /// `true` si repetir la operación puede funcionar.
        retryable: bool,
    },
    /// Cancelado por el usuario o por shutdown; estado limpio garantizado.
    #[error("cancelled")]
    Cancelled,
    /// El policy engine denegó la operación (agentes/plugins, M4).
    #[error("denied by policy rule `{rule}`")]
    PolicyDenied {
        /// Identificador de la regla que denegó.
        rule: String,
    },
    /// Una transcodificación habría perdido datos y se abortó.
    #[error("encoding loss")]
    EncodingLoss,
    /// La operación no está soportada por este provider (ver `Capabilities`).
    #[error("unsupported operation")]
    Unsupported,
    /// El `VPath` recibido no parsea o viola invariantes.
    #[error("invalid path")]
    InvalidPath,
    /// Error interno del core; `panic: true` = task supervisada que reventó
    /// (el daemon sigue vivo, spec §17.7).
    #[error("internal error (panic: {panic})")]
    Internal {
        /// `true` si el origen fue un panic capturado en una task.
        panic: bool,
    },
    /// Ciclo de symlinks detectado al recorrer con `Follow` (visited set
    /// de la spec §17.9; issue #31). Un cliente N-1 degrada a `Unknown`.
    #[error("symlink loop")]
    Loop,
    /// Categoría de un protocolo más nuevo (fallback de deserialización).
    /// El core JAMÁS la emite; existe para que un cliente N degrade con
    /// elegancia ante categorías N+1.
    #[doc(hidden)]
    #[error("unknown error category (newer protocol)")]
    #[serde(other)]
    Unknown,
}
