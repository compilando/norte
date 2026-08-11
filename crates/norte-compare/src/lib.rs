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
//!
//! El vocabulario de las filas —veredicto, criterio y confianza— vive en
//! `norte-proto` y se reexporta aquí para que quien use el motor no tenga que
//! depender del wire a mano.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod key;

pub use key::{PairKey, PairName, SideIndex, Sides, index_side, key_for};

pub use norte_proto::methods::{
    COMPARE_MAX_DIR_ENTRIES, COMPARE_ROWS_MAX_BATCH, CompareConfidence, CompareCriteria,
    CompareCriterion, CompareReason, CompareRow, CompareVerdict, Side,
};

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
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CompareOptions {
    /// Qué rungs corren. El caro (`hash`) es opt-in.
    pub criteria: CompareCriteria,
    /// Profundidad máxima del descenso, contando la raíz como 0. `None` = sin
    /// límite.
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
    pub follow_symlinks: bool,
}

impl Default for CompareOptions {
    fn default() -> Self {
        Self {
            criteria: CompareCriteria::default(),
            max_depth: None,
            mtime_tolerance_ms: 2000,
            follow_symlinks: false,
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
}
