//! `norte-sync`: convierte las filas de comparación de la spec 1 en un PLAN
//! (spec `docs/superpowers/specs/2026-08-11-directory-sync-design.md`, ADR
//! 0049).
//!
//! Es un **transductor**, no un recorrido:
//!
//! ```text
//! Stream<CompareRow> + capabilities de los dos lados + SyncOptions
//!     →  Stream<PlanItem>
//! ```
//!
//! No abre un fichero, no lista un directorio y no toca un provider: lo único
//! que sabe de ellos son los dos booleanos que [`SyncOptions`] trae ya
//! resueltos —¿tiene papelera el destino?, ¿se puede escribir en él?— leídos
//! UNA vez de sus `Capabilities` antes de empezar. Eso es lo que hace que la
//! matriz entera —cinco clases de paso × dos modos × papelera/sin papelera ×
//! tres confianzas— se pueda probar exhaustivamente sin levantar un daemon.
//!
//! No muta nada: planificar no escribe un byte. Quien ejecuta el plan
//! —`norte_core::sync`— es quien pasa por el journal y por el motor de policy
//! (reglas duras 4 y 9).
//!
//! El vocabulario del plan vive en `norte-proto` porque viaja por el wire, y
//! se reexporta aquí para que quien use el planificador no tenga que depender
//! del protocolo a mano.
//!
//! Junto al transductor va el [`PlanHasher`]: el `plan_hash` que resume lo que
//! un humano aprueba, calculado en STREAMING sobre el mismo flujo (memoria
//! O(1), sin juntar el plan). Los CONTADORES viven en `norte-proto`, con el
//! tipo que viaja: [`SyncCounts::add`].

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod hash;
pub mod plan;

pub use hash::PlanHasher;
pub use plan::{DestWitness, PlanItem, plan};

pub use norte_proto::methods::{
    OnUnknown, PlanHash, RelPath, Side, StepReversal, SyncBlocker, SyncBlockerKind,
    SyncCompareOptions, SyncCounts, SyncMode, SyncReason, SyncStep, SyncStepKind,
};

use norte_proto::VPath;

/// Todo lo que el transductor necesita y las filas NO llevan.
///
/// Las dos raíces son absolutas y pueden ser de providers distintos; el `rel`
/// de cada paso es relativo a las dos ([`RelPath`]), que es justo lo que
/// permite que un plan de `file://` a `sftp://` sea un solo vocabulario.
///
/// ```
/// use norte_proto::VPath;
/// use norte_sync::{OnUnknown, Side, SyncMode, SyncOptions};
/// let o = SyncOptions {
///     source_root: VPath::parse("file:///origen").expect("path"),
///     dest_root: VPath::parse("file:///destino").expect("path"),
///     mode: SyncMode::Update,
///     on_unknown: OnUnknown::Copy,
///     source_side: Side::Left,
///     dest_has_trash: true,
///     dest_writable: true,
/// };
/// assert_eq!(o.mode, SyncMode::Update);
/// ```
///
/// # Serializable, y aun así NO es un tipo de wire
/// Lleva `Serialize`/`Deserialize` por UNA razón: el spool de
/// `norte_core::sync` retiene el plan aprobado en un fichero, y el ejecutor
/// necesita las dos raíces —`sync.apply` no lleva más que el hash, a propósito
/// (ADR 0049)—. Ese fichero lo escribe y lo lee el MISMO binario dentro de la
/// ventana de `SYNC_PLAN_TTL_MS`: no viaja por ningún socket, no está en el
/// JSON Schema publicado y ningún peer lo parsea, así que añadir un campo aquí
/// no es un cambio de protocolo.
///
/// `deny_unknown_fields` porque un spool que no se entiende ENTERO no se
/// entiende: un plan a medio interpretar autoriza escrituras que nadie aprobó.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyncOptions {
    /// De dónde salen los bytes.
    pub source_root: VPath,
    /// …y a dónde van. El `rel` de cada paso es relativo a estas dos.
    ///
    /// El `rel` se calcula sobre `source_root` y el ejecutor lo pega sobre
    /// este: que sea LEGAL bajo el origen no lo hace legal bajo el destino, y
    /// el planificador todavía no lo comprueba. Un nombre NFC de 172 bytes
    /// ocupa 258 al descomponerse (fixture `name_max_nfd_overflow`, por encima
    /// del `NAME_MAX` de ext4 y APFS), `CON` y un punto final no son nombres en
    /// Windows, y `f:ads` sobre NTFS escribe un flujo alternativo en vez de un
    /// fichero. Hoy eso falla al EJECUTAR, sobre un plan que el humano ya
    /// aprobó; ni [`SyncBlockerKind`] ni [`SyncReason`] tienen todavía
    /// vocabulario para decirlo antes.
    pub dest_root: VPath,
    /// Qué hace el plan con lo que sobra en el destino ([`SyncMode`]).
    ///
    /// [`SyncMode::Update`] no borra nada; [`SyncMode::Mirror`] convierte cada
    /// huérfano del destino en UN [`SyncStepKind::DeleteTree`]. Un modo que este
    /// planificador no conozca —solo lo puede añadir una versión futura de
    /// `norte-proto`, porque el wire rechaza los que no nombra— es
    /// [`SyncError::ModeNotPlanned`] y no degrada a ninguno de los dos.
    pub mode: SyncMode,
    /// Qué hace con una fila cuya confianza es
    /// [`CompareConfidence::Unknown`](norte_proto::methods::CompareConfidence::Unknown).
    pub on_unknown: OnUnknown,
    /// Cuál de los dos lados de una [`CompareRow`](norte_proto::methods::CompareRow)
    /// es el ORIGEN.
    ///
    /// La comparación es simétrica y la sincronización no. El frontend, que es
    /// quien sabe en qué panel estaba el usuario, tradujo la dirección UNA vez;
    /// a partir de aquí es un hecho y no una convención que cada capa vuelva a
    /// interpretar.
    ///
    /// [`Side::Unknown`] no nombra ningún lado: no hay origen, así que no hay
    /// plan ([`SyncError::SourceSideUnknown`]). Es lo que produciría un `"lft"`
    /// que hubiera llegado hasta aquí, y termina el flujo en vez de servir en
    /// silencio un plan vacío.
    pub source_side: Side,
    /// ¿Tiene papelera el provider del DESTINO? Decide el
    /// [`StepReversal`] de cada sobrescritura y de cada borrado, y por tanto
    /// cuántos pasos el humano verá marcados como irreversibles ANTES de
    /// aprobar (regla dura 4).
    pub dest_has_trash: bool,
    /// ¿Se puede escribir en el destino? Un destino de solo lectura no produce
    /// pasos, produce un bloqueo.
    pub dest_writable: bool,
}

/// Lo único que puede terminar un plan antes de tiempo.
///
/// Lo que NO está aquí, a propósito: un nombre que colisiona, una entrada
/// ilegible o un destino de solo lectura. Eso es un
/// [`SyncStepKind::Skip`] o un [`SyncBlocker`], y el plan sigue — un árbol de
/// tres horas no se muere en la hoja 40 000, igual que no lo hace la
/// comparación de la que sale.
///
/// ```
/// use norte_sync::SyncError;
/// assert_eq!(SyncError::Cancelled.to_string(), "planificación cancelada");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SyncError {
    /// El token de la Task se disparó (regla dura 3), o lo hizo el de la
    /// comparación que alimenta el flujo. Se emite UNA vez y el flujo termina.
    ///
    /// No hay nada que limpiar: planificar no escribe un byte.
    #[error("planificación cancelada")]
    Cancelled,
    /// [`SyncOptions::source_side`] es [`Side::Unknown`]: no nombra ningún
    /// lado, así que ninguna fila tiene origen.
    ///
    /// Es un fallo del LLAMANTE, no de los datos, y por eso mata el plan en vez
    /// de saltarse las filas: un plan vacío se aprueba igual de fácil que uno
    /// lleno, y no haber copiado nada porque el modo venía con una errata es
    /// exactamente el fallo silencioso que ADR 0048 prohíbe.
    #[error("el origen no nombra ningún lado")]
    SourceSideUnknown,
    /// Una fila trae una ruta que no cuelga de la raíz que le tocaba, así que
    /// no hay `rel` que calcular.
    ///
    /// Con las filas que produce `norte-compare` sobre las raíces de este plan
    /// no puede pasar; con otras (un llamante que emparejó mal las raíces con
    /// el flujo, un provider que devuelve rutas de otro árbol) sí. Termina el
    /// plan: escribir bajo el destino con un `rel` inventado es la clase de
    /// fallo que esta spec existe para no tener.
    ///
    /// Cubre también el caso —imposible en la práctica, porque un [`VPath`] ya
    /// los validó— de un segmento que no vuelve a validar como
    /// [`Segment`](norte_proto::Segment).
    ///
    /// Los dos [`VPath`] van en `Box` porque un plan devuelve este error dentro
    /// de un `Result` que se mueve por fila: dos rutas inline hacen del `Err`
    /// más de 128 bytes y engordan el camino FELIZ (`clippy::result_large_err`).
    ///
    /// El mensaje usa [`VPath::display_lossy`] —jamás bytes crudos hacia un
    /// terminal (issue #21)—, y eso tiene un precio que quien lo registre debe
    /// compensar: NFC y NFD se pintan igual, un espacio o un punto final no se
    /// ven, y dos bytes inválidos distintos colapsan en el mismo `�`. La causa
    /// más habitual es justamente una de esas, así que quien lo logue debe
    /// adjuntar también las formas wire ([`VPath::to_wire`], lossless) como
    /// campos de `tracing`.
    #[error("la ruta {} no cuelga de {}", .path.display_lossy(), .root.display_lossy())]
    OutsideRoot {
        /// La raíz bajo la que se esperaba encontrarla.
        root: Box<VPath>,
        /// La ruta que llegó.
        path: Box<VPath>,
    },
    /// Una fila produciría un paso cuyo `rel` es la RAÍZ ([`RelPath::is_root`]):
    /// su ruta es exactamente la raíz del plan, no algo bajo ella.
    ///
    /// Un paso que actúa sobre la raíz del destino la sobrescribe o la borra
    /// ENTERA, y ese es el blanco más destructivo del plan. Pasa con un
    /// llamante cuyas raíces de [`SyncOptions`] son más profundas que las de la
    /// comparación que alimenta el flujo, y con la fila de error que el walk
    /// emite cuando no puede listar la propia raíz.
    ///
    /// No se salta la fila, se termina el plan: las raíces con las que se
    /// comparó y las raíces con las que se planifica tienen que ser las mismas,
    /// y que no lo sean invalida todos los `rel`, no solo este.
    #[error("la raíz {} no es un paso: un paso nombra algo BAJO ella", .root.display_lossy())]
    RootIsNotAStep {
        /// La raíz sobre la que se iba a actuar.
        root: Box<VPath>,
    },
    /// El modo pedido no lo sabe planificar este binario.
    ///
    /// [`SyncMode::Update`] y [`SyncMode::Mirror`] tienen tabla; esta variante
    /// es el comodín que [`SyncMode`] obliga a escribir por ser
    /// `#[non_exhaustive]`, y lo que hace es NEGARSE. No es alcanzable desde el
    /// wire —un modo que este peer no nombra muere en el deserializador, que por
    /// eso no lleva `#[serde(other)]`—, así que solo la alcanza un
    /// `norte-proto` futuro que añada un modo sin que este crate se entere.
    ///
    /// Que ese caso caiga en un error y no en `Update` es el motivo entero de la
    /// variante: el plan de `Update` es un SUBCONJUNTO del de `Mirror`, así que
    /// quien pidiera un modo nuevo y recibiera una actualización aprobaría un
    /// plan que no hace lo que pidió sin forma de notarlo — el mismo fallo
    /// silencioso que [`SyncError::SourceSideUnknown`] evita. El comodín cae del
    /// lado de no planificar nada, igual que el de [`OnUnknown`] cae del lado
    /// de no escribir.
    #[error("este planificador no sabe planificar el modo {0:?}")]
    ModeNotPlanned(SyncMode),
    /// El flujo de filas terminó con un fallo de la comparación que no es su
    /// cancelación. Hoy no existe ninguno
    /// ([`CompareError`](norte_compare::CompareError) solo tiene `Cancelled`);
    /// la variante está para que uno futuro no se traduzca a «cancelado», que
    /// es lo que haría un comodín.
    #[error("la comparación terminó en fallo")]
    Compare(#[source] norte_compare::CompareError),
}
