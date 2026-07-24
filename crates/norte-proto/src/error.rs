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
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
    /// El policy engine denegó la operación (agentes/plugins, M3-3).
    #[error("denied by policy rule `{rule}`")]
    PolicyDenied {
        /// Categoría GRUESA de la causa de denegación — NO el identificador de
        /// la regla concreta de `policy.toml` (que no se filtra, por
        /// seguridad). Vocabulario cerrado, comparable por igualdad:
        /// `"out-of-scope"` (ruta/op fuera del scope del agente),
        /// `"scope-expired"` (el scope aplicable venció),
        /// `"policy-rule"` (una regla `deny` de `policy.toml` hizo match),
        /// `"no-rule"` (fail-closed: dentro del scope pero sin regla que
        /// aplique), `"not-approved"` (un `ask` fue denegado o su TTL venció).
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
    /// Contenedor/formato ROTO (0.17.0, #58): un zip/tar corrupto, truncado
    /// o estructuralmente mentiroso. No es un fallo de I/O (reintentar no
    /// ayuda; un fallo del provider subyacente se propaga con su propia
    /// categoría, jamás como `Corrupt`); la UX honesta es «no es un
    /// contenedor válido». Desde 0.23.0 (#95) exceder los topes anti-bomba
    /// LOCALES ya no es `Corrupt`: es [`Error::LimitExceeded`] — el
    /// contenedor puede ser perfectamente válido. Un cliente N-1 degrada a
    /// `Unknown`.
    #[error("corrupt or invalid container/format")]
    Corrupt,
    /// El contenedor excede un LÍMITE LOCAL anti-bomba del índice (0.23.0,
    /// #95, ADR 0018 D2). Distinto de [`Error::Corrupt`]: el contenedor
    /// puede ser VÁLIDO (un tar.gz legítimo enorme) — norte REHÚSA pagar su
    /// coste con los límites vigentes, no lo declara roto. Un cliente N-1
    /// degrada a `Unknown` (misma UX gruesa que el `Corrupt` de antes).
    #[error("container exceeds local limit: {limit}")]
    LimitExceeded {
        /// QUÉ límite se excedió — vocabulario CERRADO, comparable por
        /// igualdad (nunca el valor numérico, que es configuración local):
        /// [`Error::LIMIT_ENTRIES`] (entradas del índice — o anunciadas por
        /// el EOCD — por encima de `max_entries`, presupuesto de omitidas
        /// incluido), [`Error::LIMIT_DECOMPRESSED_BYTES`] (inflado
        /// acumulado por encima de `max_decompressed_bytes`) o
        /// [`Error::LIMIT_NESTING`] (capas de archivo anidadas por encima
        /// de `max_nesting`, #56/proto 0.24). Los emisores usan las
        /// constantes, jamás literales sueltos (fuente única, pin en
        /// tests). Forward-compat: un token DESCONOCIDO (peer más nuevo)
        /// se trata como límite genérico — mostrar el string tal cual,
        /// jamás fallar el parse ni adivinar.
        limit: String,
    },
    /// Host key SSH DESCONOCIDA en el primer contacto (TOFU — ADR 0015 D). El
    /// frontend muestra el `fingerprint` y, si el usuario confía, llama a
    /// `connection.trust_host_key` y reintenta. Un cliente N-1 degrada a
    /// `Unknown` (0.7.0, fase 6).
    #[error("unknown host key for {host} ({algo})")]
    HostKeyUnknown {
        /// Host DESNUDO al que se conecta (sin puerto; el puerto va aparte
        /// para que el mapeo error→`connection.trust_host_key` sea 1:1).
        host: String,
        /// Puerto (ausente = default del scheme).
        #[serde(default)]
        port: Option<u16>,
        /// Algoritmo de la clave (p. ej. `ssh-ed25519`).
        algo: String,
        /// Fingerprint en formato OpenSSH `SHA256:<base64>` — la MISMA cadena
        /// que core y frontend comparan y que va en `trust_host_key`.
        fingerprint: String,
    },
    /// La host key SSH CAMBIÓ respecto a la registrada en `known_hosts`:
    /// posible MITM. JAMÁS se acepta en silencio (0.7.0, fase 6).
    #[error("host key MISMATCH for {host} ({algo}) — possible MITM")]
    HostKeyMismatch {
        /// Host DESNUDO afectado (sin puerto).
        host: String,
        /// Puerto (ausente = default del scheme).
        #[serde(default)]
        port: Option<u16>,
        /// Algoritmo de la clave presentada.
        algo: String,
        /// Fingerprint OpenSSH `SHA256:<base64>` de la clave presentada.
        fingerprint: String,
    },
    /// Un `cursor` de paginación de `fs.list` ya no es válido: expiró (TTL),
    /// fue expulsado (LRU) o murió con la conexión (ADR 0017, 0.8.0). El
    /// cliente reinicia el listado desde cero. Un cliente N-1 (0.7.x) jamás la
    /// ve — no envía cursores — pero degrada a `Unknown` si la recibiera.
    #[error("list cursor expired; restart the listing")]
    CursorExpired,
    /// Categoría de un protocolo más nuevo (fallback de deserialización).
    /// El core JAMÁS la emite; existe para que un cliente N degrade con
    /// elegancia ante categorías N+1.
    #[doc(hidden)]
    #[error("unknown error category (newer protocol)")]
    #[serde(other)]
    Unknown,
}

impl Error {
    /// Vocabulario de [`Error::LimitExceeded`]: entradas del índice (o
    /// anunciadas por el EOCD) por encima de `max_entries`, presupuesto de
    /// omitidas incluido. Fuente ÚNICA — los emisores no escriben literales.
    ///
    /// ```
    /// use norte_proto::Error;
    /// let e = Error::LimitExceeded { limit: Error::LIMIT_ENTRIES.into() };
    /// assert_eq!(serde_json::to_string(&e).unwrap(),
    ///            r#"{"kind":"limit_exceeded","limit":"entries"}"#);
    /// ```
    pub const LIMIT_ENTRIES: &'static str = "entries";
    /// Vocabulario de [`Error::LimitExceeded`]: inflado acumulado por encima
    /// de `max_decompressed_bytes` (gzip bomb o contenedor legítimo enorme).
    pub const LIMIT_DECOMPRESSED_BYTES: &'static str = "decompressed-bytes";
    /// Vocabulario de [`Error::LimitExceeded`] (#56, proto 0.24): capas de
    /// archivo anidadas por encima de `max_nesting` (lo gobierna el engine —
    /// el direccionamiento en sí es ilimitado sintácticamente).
    pub const LIMIT_NESTING: &'static str = "nesting";
}
