//! `norte-compare`: el motor que contesta «¿son iguales estos dos árboles?» —
//! y, cuando contesta que sí, dice **cuánto vale ese sí** (ADR 0048, spec
//! `docs/superpowers/specs/2026-08-11-directory-comparison-design.md`).
//!
//! Es una función pura de dos [`Provider`](norte_vfs::Provider): no conoce el
//! daemon, ni el scheduler, ni el motor de policy, ni el journal. Entra un par
//! de raíces, sale un flujo de [`CompareRow`]. Quién acumula esas filas, quién
//! las agrupa en lotes y quién decide si el llamante tenía permiso para pedirlas
//! es asunto de `norte-core`.
//!
//! No muta nada: la comparación no escribe un solo byte, así que no entra en el
//! journal (regla dura 4 no aplica, y decirlo aquí ahorra la pregunta).
//!
//! Las piezas, de abajo arriba:
//!
//! - [`key`] — el emparejamiento: qué nombre de un lado se mide contra qué
//!   nombre del otro, y qué dos nombres de un mismo lado colapsan en uno. Los
//!   bytes originales de cada nombre sobreviven intactos (regla dura 1): la
//!   clave existe SOLO para emparejar.
//! - [`cascade`] — la decisión: una pareja emparejada entra y sale un
//!   veredicto, el rung que lo decidió y lo que ese rung vale. Pura y
//!   síncrona: lo que exige I/O (destino de un symlink, sha256) entra ya
//!   averiguado.
//! - `hash` (privado) — el rung caro: el sha256 en streaming de un fichero.
//!   No se publica porque el motor no ofrece «hashea esto», ofrece
//!   [`CompareOptions::with_hash`].
//! - [`walk`] — el recorrido: dos raíces entran y sale el flujo de filas.
//!   Profundidad primero con pila explícita, directorio contra directorio,
//!   con los errores convertidos en filas y la cancelación como único final
//!   prematuro.
//!
//! El vocabulario de las filas —veredicto, criterio y confianza— vive en
//! `norte-proto` y se reexporta aquí para que quien use el motor no tenga que
//! depender del wire a mano.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod cascade;
mod hash;
pub mod key;
pub mod walk;

pub use cascade::{Decision, HashOutcome, Prefetched, decide};
pub use key::{PairKey, PairName, SideIndex, Sides, index_side, key_for};
pub use walk::{CompareStream, compare};

pub use norte_proto::methods::{
    COMPARE_MAX_DIR_ENTRIES, COMPARE_ROWS_MAX_BATCH, CompareConfidence, CompareCriteria,
    CompareCriterion, CompareReason, CompareRow, CompareVerdict, Side,
};

/// Lo único que puede terminar una comparación antes de tiempo.
///
/// Los fallos de verdad —un subdirectorio ilegible, un directorio
/// desmesurado, una lectura rota a mitad de hash— NO están aquí: son filas
/// [`CompareVerdict::Error`], y el walk sigue. Una comparación de tres horas no
/// puede morirse en el `EACCES` de la hoja 40 000.
///
/// ```
/// use norte_compare::CompareError;
/// assert_eq!(CompareError::Cancelled.to_string(), "comparación cancelada");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, thiserror::Error)]
#[non_exhaustive]
pub enum CompareError {
    /// El token de la Task se disparó (regla dura 3). El flujo lo emite UNA
    /// vez y termina; sirve para distinguir «el árbol se acabó» de «se cortó»
    /// sin un segundo canal que decirlo.
    ///
    /// No hay nada que limpiar: la comparación no escribe un solo byte.
    #[error("comparación cancelada")]
    Cancelled,
}

/// Qué rungs de la cascada corren, y bajo qué tolerancia.
///
/// Es el [`FsCompareParams`](norte_proto::methods::FsCompareParams) del wire
/// menos las dos raíces: el motor las recibe aparte, junto a sus providers.
///
/// ```
/// use norte_compare::CompareOptions;
/// let o = CompareOptions::cheap();
/// assert_eq!(o.mtime_tolerance_ms, 2000, "la regla FAT");
/// assert!(!o.criteria.hash, "el rung que LEE contenido es siempre explícito");
/// assert!(!o.follow_symlinks);
/// assert!(o.descend_orphans.is_none(), "un huérfano es UNA fila salvo que se pida lo contrario");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CompareOptions {
    /// Qué rungs corren. El caro (`hash`) es opt-in.
    pub criteria: CompareCriteria,
    /// Profundidad máxima del DESCENSO, contando la raíz como 0. `None` = sin
    /// límite.
    ///
    /// Es la profundidad del directorio que se empareja, no la de las filas:
    /// con `Some(0)` se empareja solo la raíz, lo que emite las filas de sus
    /// hijos directos y no baja a ninguno.
    pub max_depth: Option<u32>,
    /// Tolerancia del rung de mtime, en milisegundos. Default 2000 (la regla
    /// FAT, la granularidad real más ancha que un filesystem de los que este
    /// árbol toca puede tener).
    ///
    /// `u32` y NO `i64`: una tolerancia negativa hace que `|Δ| > tolerancia`
    /// sea cierto para TODA pareja, o sea una comparación entera contestando
    /// «distinto» por una errata. El tipo lo impide, aquí y en el wire
    /// (hallazgo MAJOR de `protocol-guardian`, revisión de C1).
    pub mtime_tolerance_ms: u32,
    /// Seguir symlinks. Default `false`, y la spec lo deja fuera: los destinos
    /// se comparan COMO BYTES, con lo que no hace falta detectar ciclos.
    ///
    /// **Se acepta y se IGNORA**: ponerlo a `true` no cambia ni una fila, y no
    /// hay nada en el motor que lo lea. Está aquí porque el campo existe en el
    /// wire; quien atienda `fs.compare` debe rechazar `true` con
    /// `INVALID_PARAMS` en vez de aceptar en silencio una petición que no va a
    /// cumplir.
    pub follow_symlinks: bool,
    /// Descender en los directorios que existen SOLO en este lado. `None` —el
    /// default, y lo único que la spec 1 sabía hacer— emite UNA fila por el
    /// huérfano y no lo recorre.
    ///
    /// Como opción de comparación se sostiene sola («enséñame todo lo que solo
    /// está a la izquierda, no solo la punta»), pero quien la pidió es el plan
    /// de sincronización: quien lo aprueba necesita saber CUÁNTOS ficheros hay
    /// dentro del huérfano del ORIGEN, y el ejecutor un paso por fichero para
    /// journalizar y para aislar un fallo a un solo fichero.
    ///
    /// Ficheros, no bytes: una fila huérfana no se hidrata —a ella no la mira
    /// ningún rung—, así que sobre un provider perezoso (`file://` entre ellos)
    /// su `size` viene vacío y sumar los bytes de un plan exige `stat`earlos
    /// aparte (<https://github.com/compilando/norte/issues/157>).
    ///
    /// La fila del contenedor SIGUE saliendo, y sale antes que las de dentro.
    /// No lleva marca de «este viene descendido» porque no hace falta: el
    /// descenso es una opción de la PETICIÓN, así que quien lo pidió ya sabe
    /// que detrás del directorio vienen sus hijos, y quien no lo pidió recibe
    /// la fila de siempre.
    ///
    /// **UN lado, no los dos**, y el tipo lo impone. En el destino de una
    /// sincronización un huérfano es un borrado de árbol entero: un movimiento
    /// a la papelera, una entrada de journal y una cosa que restaurar. Partirlo
    /// en cuarenta mil pasos empeora el undo y cuesta cuarenta mil listados
    /// para no cambiar un solo paso del plan.
    ///
    /// Lo que el descenso NO cambia: `max_depth` sigue acotando (lo que se
    /// acota es el número de listados, venga de una pareja o de un huérfano),
    /// el techo de [`COMPARE_MAX_DIR_ENTRIES`] sigue siendo por directorio, un
    /// listado ilegible sigue siendo su fila, y un huérfano AMBIGUO no se
    /// desciende — igual que un directorio ilegible se lleva su subárbol.
    ///
    /// Y una que sorprende: dentro de un huérfano se sigue plegando con las
    /// capabilities de LOS DOS lados ([`Sides::from_capabilities`]), aunque el
    /// otro lado no tenga nada ahí. Dos nombres que el otro lado no sabría
    /// distinguir salen `Ambiguous` dentro del huérfano, y es lo correcto para
    /// lo que la opción existe: son exactamente los dos ficheros que no se
    /// podrían escribir juntos en el destino.
    ///
    /// [`Side::Unknown`] no es ningún lado, así que no desciende nada. Es lo
    /// que produce un `"lft"` en el wire (`Side` degrada con `serde(other)`),
    /// y por eso quien atiende `fs.compare` lo rechaza con `INVALID_PARAMS` en
    /// vez de servir en silencio un conjunto de filas distinto del pedido.
    pub descend_orphans: Option<Side>,
}

impl Default for CompareOptions {
    fn default() -> Self {
        Self {
            criteria: CompareCriteria::default(),
            max_depth: None,
            mtime_tolerance_ms: 2000,
            follow_symlinks: false,
            descend_orphans: None,
        }
    }
}

impl CompareOptions {
    /// La comparación que NO lee contenido: tamaño y fecha, sin hash.
    ///
    /// Es el default, con el rung caro apagado de forma explícita para que se
    /// vea en el sitio donde se usa.
    #[must_use]
    pub fn cheap() -> Self {
        Self {
            criteria: CompareCriteria {
                hash: false,
                ..CompareCriteria::default()
            },
            ..Self::default()
        }
    }

    /// Acota el descenso: la raíz es 0, así que `max_depth(1)` empareja la raíz
    /// y sus hijos directos, y no baja más.
    ///
    /// Comparte nombre con el campo a propósito (son espacios de nombres
    /// distintos): quien construye opciones escribe `.max_depth(1)` y quien las
    /// lee escribe `opts.max_depth`.
    ///
    /// ```
    /// use norte_compare::CompareOptions;
    /// assert_eq!(CompareOptions::cheap().max_depth(1).max_depth, Some(1));
    /// assert_eq!(CompareOptions::cheap().max_depth, None, "por defecto, sin tope");
    /// ```
    #[must_use]
    pub fn max_depth(self, depth: u32) -> Self {
        Self {
            max_depth: Some(depth),
            ..self
        }
    }

    /// Enciende el rung caro: sha256 en streaming de las parejas que los rungs
    /// baratos dieron por IGUALES.
    ///
    /// Es lo único de esta struct que LEE contenido, y por eso es explícito y
    /// no un default: nadie hashea un terabyte por SFTP sin haberlo pedido.
    ///
    /// ```
    /// use norte_compare::CompareOptions;
    /// assert!(CompareOptions::cheap().with_hash().criteria.hash);
    /// assert!(!CompareOptions::cheap().criteria.hash, "sigue siendo opt-in");
    /// ```
    #[must_use]
    pub fn with_hash(self) -> Self {
        Self {
            criteria: CompareCriteria {
                hash: true,
                ..self.criteria
            },
            ..self
        }
    }
}
