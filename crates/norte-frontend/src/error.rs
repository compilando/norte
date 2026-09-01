//! Vocabulario de CATEGORÍAS de error del protocolo, compartido por los dos
//! frontends (#158, revisión de la fase C1 — MAJOR-3).
//!
//! Vivía en `norte-tui`, así que la GUI no podía alcanzarlo y acababa
//! interpolando el `Display` INGLÉS del [`Error`] en frases por lo demás
//! localizadas. Es el mismo argumento que movió aquí
//! [`compare::CompareView`](crate::compare::CompareView): un error se dice
//! igual en las dos superficies, o la que se quede atrás miente en el idioma
//! del lector.
//!
//! Dos razones para que sea una CATEGORÍA y no el `Display`:
//!
//! * el `Display` no está traducido, y Fluent no tiene ninguna oportunidad de
//!   traducirlo;
//! * varias variantes interpolan datos del PEER —`HostKeyUnknown` lleva host,
//!   algoritmo y huella; `LimitExceeded` su límite—, y un host arbitrario en
//!   una barra es un vector bidi/control. La clave estable los descarta por
//!   patrón, así que no hay nada que sanear.

use norte_i18n::{Lang, t_in};
use norte_proto::Error;

/// Clave Fluent ESTABLE de la CATEGORÍA de un [`Error`] del protocolo (spec
/// §17.7, #20). Es la base de [`error_category`] y también el vocabulario que
/// ven los scripts Lua (`nil, clave` — M4 Lua): el script compara contra
/// claves estables, jamás contra texto localizado. Los campos con detalle
/// (host, `rule`, retryable…) se DESCARTAN por patrón: `PolicyDenied` no
/// expone la regla concreta (vocabulario cerrado); `HostKeyUnknown`/
/// `Mismatch` no filtran el host (además un `Display` con host arbitrario
/// sería un vector bidi/control en la barra). Una categoría futura
/// (`Unknown`, cliente N-1) cae a `err-unknown`.
///
/// ```
/// use norte_frontend::error::error_key;
/// assert_eq!(error_key(&norte_proto::Error::PermissionDenied), "err-permission-denied");
/// // El detalle del peer se descarta: la clave es la MISMA para todo host.
/// assert_eq!(error_key(&norte_proto::Error::NotFound), "err-not-found");
/// ```
#[must_use]
pub fn error_key(e: &Error) -> &'static str {
    use norte_proto::{ConflictKind, RootOverlap};
    match e {
        Error::NotFound => "err-not-found",
        Error::PermissionDenied => "err-permission-denied",
        Error::Conflict { conflict } => match conflict {
            ConflictKind::Exists => "err-conflict-exists",
            ConflictKind::CaseCollision => "err-conflict-case",
            ConflictKind::Normalization => "err-conflict-normalization",
            ConflictKind::TypeMismatch => "err-conflict-type",
            _ => "err-conflict",
        },
        Error::ProviderUnavailable { .. } => "err-provider-unavailable",
        Error::NoSpace => "err-no-space",
        Error::Io { .. } => "err-io",
        Error::Cancelled => "err-cancelled",
        Error::PolicyDenied { .. } => "err-policy-denied",
        Error::EncodingLoss => "err-encoding-loss",
        Error::Unsupported => "err-unsupported",
        Error::InvalidPath => "err-invalid-path",
        Error::Internal { .. } => "err-internal",
        Error::Loop => "err-loop",
        Error::Corrupt => "err-corrupt",
        // #95.3: límite local ≠ corrupción. El sub-vocabulario (`entries`/
        // `decompressed-bytes`) es diagnóstico, no UX: una sola clave.
        Error::LimitExceeded { .. } => "err-limit-exceeded",
        Error::HostKeyUnknown { .. } => "err-host-key-unknown",
        // 0.63.0 (#325): la TUI lo intercepta y abre el diálogo, así que este
        // texto solo lo ven los frontends que aún no preguntan (la CLI, y la
        // ventana hasta #327). Tiene que decir qué hacer sin diálogo — poner
        // la variable de entorno—, no «error desconocido».
        Error::SecretNeeded { .. } => "err-secret-needed",
        Error::HostKeyMismatch { .. } => "err-host-key-mismatch",
        Error::CursorExpired => "err-cursor-expired",
        // 0.36.0 (batch rename): las dos son ACCIONABLES — caer en
        // `err-unknown` sería lo contrario de lo que su rustdoc promete.
        Error::PlanStale => "err-plan-stale",
        Error::PlanNotExecutable => "err-plan-not-executable",
        // 0.40.0 (sincronización): las TRES relaciones se pintan distinto y la
        // primera no es un caso degenerado de las otras dos, así que el
        // sub-vocabulario sí viaja —igual que el de `Conflict`—. Lo accionable
        // es distinto en cada una: con `Same` hay que elegir otro directorio,
        // con las otras dos hay que salir del árbol que contiene al otro. Sin
        // este brazo la negativa caía en `err-unknown`, que es exactamente lo
        // que la variante existe para no ser.
        Error::OverlappingRoots { relation } => match relation {
            RootOverlap::Same => "err-overlapping-roots-same",
            RootOverlap::SourceInsideDest => "err-overlapping-roots-source-inside",
            RootOverlap::DestInsideSource => "err-overlapping-roots-dest-inside",
            _ => "err-overlapping-roots",
        },
        // 0.41.0 (#178): el journal de esta sesión no se puede abrir y la
        // mutación se rehusó. Es de la familia accionable —hay UN fichero que
        // arreglar o quitar— y caer en «error desconocido» dejaría al usuario
        // ante una sesión que de pronto no muta y sin decirle por qué.
        Error::JournalUnavailable => "err-journal-unavailable",
        _ => "err-unknown",
    }
}

/// Texto LOCALIZADO de la categoría de un [`Error`] del protocolo en la
/// lengua que se PIDE: la clave estable de [`error_key`] pasada por Fluent —
/// jamás el `Display` inglés hardcodeado ni un string del OS.
///
/// Existe por lo mismo que `t_in` frente a `t`: quien compone una frase con
/// un `lang` explícito —[`compare::status_line`](crate::compare::status_line)
/// lo recibe— no puede rellenar una de sus piezas con la lengua AMBIENTE, o
/// devuelve media frase traducida (revisión de la fase C1, MINOR-6).
///
/// ```
/// use norte_frontend::error::error_category_in;
/// use norte_i18n::Lang;
/// let es = error_category_in(Lang::Es, &norte_proto::Error::NotFound);
/// let en = error_category_in(Lang::En, &norte_proto::Error::NotFound);
/// // La MISMA clave, dicha en dos idiomas: ninguno de los dos es la clave.
/// assert!(!es.starts_with("err-") && !en.starts_with("err-"));
/// ```
#[must_use]
pub fn error_category_in(lang: Lang, e: &Error) -> String {
    t_in(lang, error_key(e))
}

/// [`error_category_in`] en la lengua AMBIENTE. Envoltorio, para quien no
/// tiene un `lang` que pasar.
#[must_use]
pub fn error_category(e: &Error) -> String {
    error_category_in(norte_i18n::active(), e)
}
