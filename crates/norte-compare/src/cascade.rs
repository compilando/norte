//! La cascada: entra UNA pareja ya emparejada, sale UN veredicto — y con él,
//! qué rung lo decidió y **cuánto vale ese rung**.
//!
//! Va de barato a caro y **para en el primer rung que decide**, así que el
//! criterio es también «hasta dónde hubo que llegar»: sin él, `Different` no
//! dice si se comparó un `u64` o 40 GB de bytes. La tabla es la de la spec
//! (`docs/superpowers/specs/2026-08-11-directory-comparison-design.md`,
//! «The cascade»), y las CONFIANZAS son la parte que hay que leer despacio:
//!
//! | rung | condición | veredicto | confianza |
//! | --- | --- | --- | --- |
//! | Presence | falta un lado | `OnlyLeft`/`OnlyRight` | `Certain` |
//! | Kind | los `EntryKind` difieren | `TypeMismatch` | `Certain` |
//! | Symlink | destinos, COMO BYTES | `Same`/`Different` | `Certain` |
//! | Size | los dos conocidos y distintos | `Different` | `Certain` |
//! | Size | uno desconocido | `Same` | `Unknown` |
//! | Mtime | \|Δ\| > tolerancia | `Different` | `Probable` |
//! | Mtime | \|Δ\| ≤ tolerancia | `Same` | `Probable` |
//! | Mtime | desconocida en un lado | `Same` | `Unknown` |
//! | Hash | sha256 en streaming de los dos | `Same`/`Different` | `Certain` |
//!
//! Un tamaño distinto PRUEBA bytes distintos; una fecha distinta solo lo
//! SUGIERE; y un provider que no puede contestar deja `Unknown`, que es una
//! respuesta y no un fallo. Bajar una fila de `Unknown` a `Probable` porque
//! «algo habrá que pintar» es exactamente el error que este módulo existe para
//! no cometer.
//!
//! # Por qué [`decide`] es SÍNCRONA
//!
//! Porque los dos únicos hechos que exigen I/O —el destino de un symlink
//! (`Provider::read_link`) y el sha256 del contenido— **entran ya calculados**,
//! en [`Prefetched`]. Quien camina el árbol (`walk`) sabe pedirlos; la cascada
//! solo decide. Así la tabla entera se prueba sin un provider, sin un runtime
//! async y sin un mock que conteste lo que se le mande.

use norte_proto::{Entry, EntryKind};

use crate::{
    CompareConfidence, CompareCriterion, CompareOptions, CompareRow, CompareVerdict, PairName, Side,
};

/// Lo que el rung caro (sha256) tiene YA dicho sobre una pareja cuando
/// [`decide`] la mira.
///
/// [`decide`] no lee contenido: leerlo es asíncrono y caro, y decidir es ni una
/// cosa ni la otra. El walk pregunta primero con [`HashOutcome::NotRun`], mira
/// [`Decision::needs_hash`], y solo si viene a `true` lee los dos ficheros y
/// vuelve a preguntar con el resultado.
///
/// ```
/// use norte_compare::cascade::HashOutcome;
/// assert_eq!(HashOutcome::default(), HashOutcome::NotRun, "nadie ha leído nada todavía");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum HashOutcome {
    /// Nadie ha hasheado nada: o el llamante no pidió el rung, o aún no ha
    /// llegado a leer.
    #[default]
    NotRun,
    /// Los dos sha256 coinciden.
    Equal,
    /// Los dos sha256 difieren.
    Differ,
}

/// Los hechos que [`decide`] no puede averiguar por sí misma, ya averiguados.
///
/// Son exactamente dos, y los dos exigen I/O: el destino de un symlink y el
/// sha256 del contenido. Los destinos se comparan **como bytes** y jamás se
/// siguen (sin seguimiento no hace falta detectar ciclos, y un enlace cuyo
/// destino cambió es una diferencia real).
///
/// Un destino a `None` significa «no se pudo leer / no se leyó», y la cascada
/// contesta `Unknown` en vez de inventarse que los enlaces son iguales.
///
/// ```
/// use norte_compare::cascade::{HashOutcome, Prefetched};
/// let nada = Prefetched::none();
/// assert_eq!(nada.hash, HashOutcome::NotRun);
/// assert!(nada.left_target.is_none());
///
/// let enlaces = Prefetched::links(Some(b"../a"), Some(b"../b"));
/// assert_eq!(enlaces.right_target, Some(&b"../b"[..]));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Prefetched<'a> {
    /// Destino del symlink izquierdo, en bytes crudos. `None` = no se leyó o
    /// no se pudo leer.
    pub left_target: Option<&'a [u8]>,
    /// Destino del symlink derecho, en bytes crudos.
    pub right_target: Option<&'a [u8]>,
    /// Qué dijo el rung de hash, si es que corrió.
    pub hash: HashOutcome,
}

impl<'a> Prefetched<'a> {
    /// Nada averiguado: ni destinos ni hash. Es lo que el walk pasa en la
    /// primera pasada de CADA pareja.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            left_target: None,
            right_target: None,
            hash: HashOutcome::NotRun,
        }
    }

    /// Los dos destinos de symlink, tal y como los dio `Provider::read_link`.
    #[must_use]
    pub const fn links(left: Option<&'a [u8]>, right: Option<&'a [u8]>) -> Self {
        Self {
            left_target: left,
            right_target: right,
            hash: HashOutcome::NotRun,
        }
    }

    /// El mismo conjunto de hechos, con el resultado del rung de hash puesto.
    #[must_use]
    pub const fn with_hash(self, hash: HashOutcome) -> Self {
        Self { hash, ..self }
    }
}

/// Lo que decidió la cascada sobre una pareja: el veredicto, el rung que lo
/// produjo y lo que ese rung se ha ganado.
///
/// No es todavía una [`CompareRow`]: le faltan el id y los dos `Entry`, que
/// pone el walk ([`Decision::into_row`]).
///
/// ```
/// use norte_compare::cascade::Decision;
/// use norte_compare::{CompareConfidence, CompareCriterion, CompareVerdict};
///
/// // La presencia es el rung más barato y el más seguro: si un lado no está,
/// // no hay nada más que comparar.
/// let d = Decision::only_left();
/// assert_eq!(d.verdict, CompareVerdict::OnlyLeft);
/// assert_eq!(d.criterion, CompareCriterion::Presence);
/// assert_eq!(d.confidence, CompareConfidence::Certain);
/// assert!(!d.needs_hash);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Decision {
    /// Qué son la una respecto de la otra.
    pub verdict: CompareVerdict,
    /// Qué rung lo decidió.
    pub criterion: CompareCriterion,
    /// Cuánto vale ese rung. NO se sube nunca «para que quede bonito».
    pub confidence: CompareConfidence,
    /// Qué lado es MÁS NUEVO, y solo cuando lo decidió el rung de fecha. Nada
    /// de esta spec lo lee: la spec 2 lo necesita para proponer una dirección,
    /// y producirlo aquí no cuesta nada.
    pub newer: Option<Side>,
    /// Los rungs baratos dieron la pareja por IGUAL, el llamante pidió hash y
    /// el hash aún no ha corrido: esta decisión **no es final**. Quien la
    /// recibe lee los dos ficheros y vuelve a llamar a [`decide`] con el
    /// [`HashOutcome`]. Emitir una fila con esto a `true` es publicar un
    /// veredicto provisional, y en esta spec ninguna fila se corrige después.
    pub needs_hash: bool,
}

impl Decision {
    /// Una decisión final de un rung que no mira fechas ni lados.
    const fn rung(
        verdict: CompareVerdict,
        criterion: CompareCriterion,
        confidence: CompareConfidence,
    ) -> Self {
        Self {
            verdict,
            criterion,
            confidence,
            newer: None,
            needs_hash: false,
        }
    }

    /// El rung de presencia: solo existe a la izquierda.
    #[must_use]
    pub const fn only_left() -> Self {
        Self::rung(
            CompareVerdict::OnlyLeft,
            CompareCriterion::Presence,
            CompareConfidence::Certain,
        )
    }

    /// El rung de presencia: solo existe a la derecha.
    #[must_use]
    pub const fn only_right() -> Self {
        Self::rung(
            CompareVerdict::OnlyRight,
            CompareCriterion::Presence,
            CompareConfidence::Certain,
        )
    }

    /// La fila del wire: esta decisión, un id y los dos lados.
    ///
    /// Los lados los pone el llamante porque solo él sabe cuál falta: una
    /// decisión de presencia lleva `None` en el suyo.
    ///
    /// ```
    /// use norte_compare::cascade::Decision;
    /// use norte_compare::CompareVerdict;
    /// # use norte_proto::{Entry, EntryKind, VPath};
    /// # let izq = Entry { path: VPath::parse("file:///a").expect("path"),
    /// #     kind: EntryKind::File, size: Some(1), mtime_ms: None, attrs: Default::default() };
    /// let fila = Decision::only_left().into_row(7, Some(izq), None);
    /// assert_eq!(fila.id, 7);
    /// assert_eq!(fila.verdict, CompareVerdict::OnlyLeft);
    /// assert!(fila.sides_are_consistent());
    /// assert!(fila.reason.is_none(), "la cascada no produce filas con motivo");
    /// ```
    #[must_use]
    pub fn into_row(self, id: u64, left: Option<Entry>, right: Option<Entry>) -> CompareRow {
        // La marca de #152 se calcula AQUÍ y no la pone la llamante porque aquí
        // están las dos entradas y no hay otro camino a una fila con los dos
        // lados: una fila que se emitiera sin pasar por este constructor no
        // podría olvidarse la marca, porque no existe.
        let paired_under = match (left.as_ref(), right.as_ref()) {
            (Some(l), Some(r)) => crate::key::pair_transform(l.pair_name(), r.pair_name()),
            _ => None,
        };
        let row = CompareRow {
            id,
            left,
            right,
            verdict: self.verdict,
            criterion: self.criterion,
            confidence: self.confidence,
            newer: self.newer,
            // La cascada no emite `Ambiguous` ni `Error`: el motivo y el lado
            // son del walk (colisiones, listados ilegibles, lecturas rotas).
            reason: None,
            side: None,
            paired_under,
        };
        // La invariante que el wire enuncia y no sabe comprobar
        // (`CompareRow::paired_under`): una transformación es propiedad de una
        // PAREJA, así que marcar una fila de un solo lado no significaría nada.
        // Hoy es cierta por construcción —el `match` de arriba—, y este assert
        // es lo que la mantiene cierta si alguien reescribe ese `match`.
        debug_assert!(
            row.paired_under.is_none() || (row.left.is_some() && row.right.is_some()),
            "transformación de emparejamiento en una fila sin dos lados"
        );
        debug_assert!(
            row.sides_are_consistent(),
            "veredicto {:?} con left={} right={}",
            row.verdict,
            row.left.is_some(),
            row.right.is_some()
        );
        row
    }
}

/// Decide UNA pareja: baja la cascada y para en el primer rung que contesta.
///
/// `facts` trae lo que exige I/O ya hecho (ver [`Prefetched`]); `opts` dice qué
/// rungs corren y con qué tolerancia. La función es pura: mismas entradas,
/// misma decisión, siempre.
///
/// Es **simétrica**: intercambiar los dos lados intercambia el veredicto
/// (`OnlyLeft`↔`OnlyRight`, que decide el walk) y el lado de
/// [`Decision::newer`], y no cambia nada más. Una comparación que no lo fuese
/// tendría un favorito.
///
/// ```
/// use norte_compare::cascade::{decide, Prefetched};
/// use norte_compare::{CompareConfidence, CompareCriterion, CompareOptions, CompareVerdict};
/// # use norte_proto::{Entry, EntryKind, VPath};
/// # fn f(size: Option<u64>, mtime: Option<i64>) -> Entry {
/// #     Entry { path: VPath::parse("file:///x").expect("path"), kind: EntryKind::File,
/// #             size, mtime_ms: mtime, attrs: Default::default() }
/// # }
/// let opts = CompareOptions::cheap();
///
/// // Tamaños distintos: PRUEBA de bytes distintos.
/// let d = decide(&f(Some(10), Some(0)), &f(Some(20), Some(0)), &opts, &Prefetched::none());
/// assert_eq!(d.criterion, CompareCriterion::Size);
/// assert_eq!(d.confidence, CompareConfidence::Certain);
///
/// // Misma fecha: solo una SUGERENCIA de que son iguales.
/// let d = decide(&f(Some(10), Some(0)), &f(Some(10), Some(500)), &opts, &Prefetched::none());
/// assert_eq!((d.verdict, d.confidence), (CompareVerdict::Same, CompareConfidence::Probable));
///
/// // Un provider que no sabe el tamaño no recibe una confianza inventada.
/// let d = decide(&f(None, Some(0)), &f(Some(10), Some(0)), &opts, &Prefetched::none());
/// assert_eq!((d.verdict, d.confidence), (CompareVerdict::Same, CompareConfidence::Unknown));
/// ```
#[must_use]
pub fn decide(
    left: &Entry,
    right: &Entry,
    opts: &CompareOptions,
    facts: &Prefetched<'_>,
) -> Decision {
    let cheap = cheap_rungs(left, right, opts, facts);

    // El rung caro alcanza SOLO a las parejas que los baratos dieron por
    // iguales —«verifica lo que parece igual»—, y solo a ficheros: un
    // directorio no tiene contenido que hashear y un symlink ya decidió por su
    // destino.
    if !opts.criteria.hash || cheap.verdict != CompareVerdict::Same || left.kind != EntryKind::File
    {
        return cheap;
    }
    match facts.hash {
        HashOutcome::NotRun => Decision {
            needs_hash: true,
            ..cheap
        },
        HashOutcome::Equal => Decision::rung(
            CompareVerdict::Same,
            CompareCriterion::Hash,
            CompareConfidence::Certain,
        ),
        HashOutcome::Differ => Decision::rung(
            CompareVerdict::Different,
            CompareCriterion::Hash,
            CompareConfidence::Certain,
        ),
    }
}

/// Los rungs que no leen contenido: kind, destino de enlace, tamaño y fecha.
fn cheap_rungs(
    left: &Entry,
    right: &Entry,
    opts: &CompareOptions,
    facts: &Prefetched<'_>,
) -> Decision {
    if left.kind != right.kind {
        return Decision::rung(
            CompareVerdict::TypeMismatch,
            CompareCriterion::Kind,
            CompareConfidence::Certain,
        );
    }

    match left.kind {
        EntryKind::File => size_and_mtime(left, right, opts),
        // Dos directorios del mismo nombre son EL MISMO directorio: sus
        // diferencias son las filas de sus hijos, que el walk emite aparte.
        // Compararlos por tamaño o por fecha sería pintar «distinto» en cada
        // directorio que contiene un fichero cambiado —la fecha de un
        // directorio se mueve con cualquier hijo— y ahogar el panel en ruido
        // que no se puede operar.
        EntryKind::Dir => Decision::rung(
            CompareVerdict::Same,
            CompareCriterion::Kind,
            CompareConfidence::Certain,
        ),
        EntryKind::Symlink => link_target(facts),
        // Un socket, un fifo, un device — o un kind de un protocolo N+1 que
        // este binario no conoce. Son del mismo tipo y ahí se acaba lo que se
        // sabe: su «tamaño» no es contenido y su fecha no dice nada.
        EntryKind::Other => Decision::rung(
            CompareVerdict::Same,
            CompareCriterion::Kind,
            CompareConfidence::Unknown,
        ),
    }
}

/// El rung de symlink: los destinos, como bytes, jamás seguidos.
fn link_target(facts: &Prefetched<'_>) -> Decision {
    match (facts.left_target, facts.right_target) {
        (Some(l), Some(r)) => Decision::rung(
            if l == r {
                CompareVerdict::Same
            } else {
                CompareVerdict::Different
            },
            CompareCriterion::LinkTarget,
            CompareConfidence::Certain,
        ),
        // Sin los dos destinos no hay comparación posible, y decir `Different`
        // sería inventarse una diferencia igual que decir `Same` se inventaría
        // una igualdad.
        _ => Decision::rung(
            CompareVerdict::Same,
            CompareCriterion::LinkTarget,
            CompareConfidence::Unknown,
        ),
    }
}

/// Los dos rungs de metadatos de un fichero, en orden.
fn size_and_mtime(left: &Entry, right: &Entry, opts: &CompareOptions) -> Decision {
    if opts.criteria.size {
        match (left.size, right.size) {
            (Some(l), Some(r)) if l != r => {
                return Decision::rung(
                    CompareVerdict::Different,
                    CompareCriterion::Size,
                    CompareConfidence::Certain,
                );
            }
            // Tamaños iguales no deciden nada: dos ficheros de 4 KiB pueden
            // tener bytes distintos. Sigue la cascada.
            (Some(_), Some(_)) => {}
            // Un lado sin tamaño: el rung no puede contestar, y el siguiente
            // tampoco lo arregla. `Same`/`Unknown` es la respuesta honesta —
            // seguir a la fecha daría `Probable`, o sea MÁS confianza de la
            // que hay.
            _ => {
                return Decision::rung(
                    CompareVerdict::Same,
                    CompareCriterion::Size,
                    CompareConfidence::Unknown,
                );
            }
        }
    }

    if opts.criteria.mtime {
        let (Some(l), Some(r)) = (left.mtime_ms, right.mtime_ms) else {
            return Decision::rung(
                CompareVerdict::Same,
                CompareCriterion::Mtime,
                CompareConfidence::Unknown,
            );
        };
        // `(l - r).abs()` DESBORDA: las fechas pre-1970 son negativas y reales,
        // y un par (i64::MIN, i64::MAX) revienta en debug y da basura en
        // release. `saturating_sub` + `unsigned_abs` no puede desbordar, y la
        // tolerancia es `u32` para que no exista una tolerancia negativa que
        // convierta la comparación entera en «distinto».
        if l.saturating_sub(r).unsigned_abs() > u64::from(opts.mtime_tolerance_ms) {
            return Decision {
                newer: Some(if l > r { Side::Left } else { Side::Right }),
                ..Decision::rung(
                    CompareVerdict::Different,
                    CompareCriterion::Mtime,
                    CompareConfidence::Probable,
                )
            };
        }
        return Decision::rung(
            CompareVerdict::Same,
            CompareCriterion::Mtime,
            CompareConfidence::Probable,
        );
    }

    // Ni tamaño ni fecha: ningún rung decidió. `Presence` es el criterio de
    // esas filas por convención del wire (ver `CompareCriterion::Presence`), y
    // la confianza es `Unknown` porque literalmente no se ha comparado nada.
    Decision::rung(
        CompareVerdict::Same,
        CompareCriterion::Presence,
        CompareConfidence::Unknown,
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use norte_proto::VPath;

    use super::*;
    use crate::CompareCriteria;

    /// `size` y `mtime` genéricos porque la MISMA tabla lleva `file(10, 0)`,
    /// `file(10, None)` y `file(None, 0)`.
    fn entry(
        kind: EntryKind,
        size: impl Into<Option<u64>>,
        mtime: impl Into<Option<i64>>,
    ) -> Entry {
        Entry {
            path: VPath::parse("file:///x").expect("path"),
            kind,
            size: size.into(),
            mtime_ms: mtime.into(),
            attrs: BTreeMap::default(),
        }
    }

    fn file(size: impl Into<Option<u64>>, mtime: impl Into<Option<i64>>) -> Entry {
        entry(EntryKind::File, size, mtime)
    }

    fn dir() -> Entry {
        entry(EntryKind::Dir, None, 1_700_000_000_000)
    }

    fn link() -> Entry {
        entry(EntryKind::Symlink, 4, 0)
    }

    /// Nada averiguado: ni destinos de enlace, ni hash.
    fn no_hash() -> Prefetched<'static> {
        Prefetched::none()
    }

    fn targets<'a>(left: &'a [u8], right: &'a [u8]) -> Prefetched<'a> {
        Prefetched::links(Some(left), Some(right))
    }

    fn with_hash() -> CompareOptions {
        CompareOptions {
            criteria: CompareCriteria {
                hash: true,
                ..CompareCriteria::default()
            },
            ..CompareOptions::cheap()
        }
    }

    /// The cascade's whole contract, one row per rung. The confidences are the
    /// point: a different size PROVES different bytes, a different mtime only
    /// suggests it, and a provider that cannot say leaves `Unknown` rather than
    /// having something invented for it.
    #[test]
    fn the_cascade_decides_and_says_how_sure_it_is() {
        use CompareConfidence as Conf;
        use CompareCriterion as C;
        use CompareVerdict::{Different, Same, TypeMismatch};
        let opts = CompareOptions::cheap(); // tolerance 2000 ms, no hash

        let cases = [
            // (left, right, verdict, criterion, confidence)
            (file(10, 0), file(20, 0), Different, C::Size, Conf::Certain),
            (file(10, 0), dir(), TypeMismatch, C::Kind, Conf::Certain),
            (file(10, 0), file(10, 1_000), Same, C::Mtime, Conf::Probable),
            (
                file(10, 0),
                file(10, 5_000),
                Different,
                C::Mtime,
                Conf::Probable,
            ),
            (file(10, 0), file(10, None), Same, C::Mtime, Conf::Unknown),
            (file(None, 0), file(10, 0), Same, C::Size, Conf::Unknown),
        ];
        for (l, r, verdict, criterion, confidence) in cases {
            let row = decide(&l, &r, &opts, &no_hash());
            assert_eq!(
                (row.verdict, row.criterion, row.confidence),
                (verdict, criterion, confidence),
                "{l:?} vs {r:?}"
            );
        }
    }

    /// Spec 2 proposes a direction from this field, so it is produced here even
    /// though nothing in this spec reads it.
    #[test]
    fn a_row_that_differs_by_mtime_records_the_newer_side() {
        let row = decide(
            &file(10, 0),
            &file(10, 9_000),
            &CompareOptions::cheap(),
            &no_hash(),
        );
        assert_eq!(row.newer, Some(Side::Right));
    }

    /// Symlinks are compared, not followed: no cycle detection needed, and a
    /// link whose target changed is a real difference.
    ///
    /// El destino NO vive en `Entry`: lo lee `Provider::read_link`, que es
    /// async, así que entra por [`Prefetched`] y `decide` sigue siendo pura.
    #[test]
    fn symlink_targets_are_compared_as_bytes() {
        let opts = CompareOptions::cheap();
        let same = decide(&link(), &link(), &opts, &targets(b"../a", b"../a"));
        assert_eq!(
            (same.verdict, same.criterion),
            (CompareVerdict::Same, CompareCriterion::LinkTarget)
        );
        let diff = decide(&link(), &link(), &opts, &targets(b"../a", b"../b"));
        assert_eq!(diff.verdict, CompareVerdict::Different);
        assert_eq!(diff.confidence, CompareConfidence::Certain);
    }

    // ---- lo que la tabla de la spec no fija, y una revisión podría torcer ----

    /// Un destino que no se pudo leer no se convierte en «son iguales»: sin los
    /// dos destinos la confianza es `Unknown`, que es una respuesta.
    #[test]
    fn un_enlace_sin_destino_leido_no_inventa_una_igualdad() {
        let opts = CompareOptions::cheap();
        for facts in [
            no_hash(),
            Prefetched::links(Some(b"../a"), None),
            Prefetched::links(None, Some(b"../a")),
        ] {
            let d = decide(&link(), &link(), &opts, &facts);
            assert_eq!(
                (d.verdict, d.criterion, d.confidence),
                (
                    CompareVerdict::Same,
                    CompareCriterion::LinkTarget,
                    CompareConfidence::Unknown
                ),
                "{facts:?}"
            );
        }
    }

    /// Dos directorios del mismo nombre son el mismo directorio: sus
    /// diferencias son las filas de sus hijos. Compararlos por fecha pintaría
    /// «distinto» en cada carpeta que contiene un fichero cambiado.
    #[test]
    fn dos_directorios_no_se_comparan_por_fecha_ni_por_tamano() {
        let izq = entry(EntryKind::Dir, 4096, 0);
        let der = entry(EntryKind::Dir, 8192, 999_999_999);
        let d = decide(&izq, &der, &CompareOptions::cheap(), &no_hash());
        assert_eq!(
            (d.verdict, d.criterion, d.confidence),
            (
                CompareVerdict::Same,
                CompareCriterion::Kind,
                CompareConfidence::Certain
            )
        );
        assert!(d.newer.is_none(), "un directorio no tiene lado más nuevo");
    }

    /// Un kind que no es fichero, ni directorio, ni enlace —un fifo, un
    /// device, o un kind de un daemon N+1— no recibe una confianza inventada.
    #[test]
    fn un_kind_desconocido_no_se_compara_por_metadatos() {
        let izq = entry(EntryKind::Other, 1, 0);
        let der = entry(EntryKind::Other, 2, 500_000);
        let d = decide(&izq, &der, &CompareOptions::cheap(), &no_hash());
        assert_eq!(
            (d.verdict, d.criterion, d.confidence),
            (
                CompareVerdict::Same,
                CompareCriterion::Kind,
                CompareConfidence::Unknown
            )
        );
    }

    /// Las fechas pre-1970 son negativas y REALES: `(l - r).abs()` desborda con
    /// ellas. La resta saturada no, y sigue contestando lo que toca.
    #[test]
    fn las_fechas_pre_1970_no_desbordan_la_resta() {
        let opts = CompareOptions::cheap();
        let d = decide(&file(10, i64::MIN), &file(10, i64::MAX), &opts, &no_hash());
        assert_eq!(d.verdict, CompareVerdict::Different);
        assert_eq!(d.newer, Some(Side::Right));

        let d = decide(&file(10, i64::MIN), &file(10, i64::MIN), &opts, &no_hash());
        assert_eq!(
            (d.verdict, d.criterion, d.confidence),
            (
                CompareVerdict::Same,
                CompareCriterion::Mtime,
                CompareConfidence::Probable
            )
        );

        // Y una fecha pre-1970 dentro de tolerancia sigue siendo la misma.
        let d = decide(
            &file(10, -1_000_000_000_000),
            &file(10, -1_000_000_001_000),
            &opts,
            &no_hash(),
        );
        assert_eq!(d.verdict, CompareVerdict::Same);
    }

    /// La tolerancia INCLUYE su extremo: la spec dice `|Δ| > tolerancia` para
    /// `Different`, y un FAT con granularidad de 2 s no puede distinguir
    /// exactamente 2000 ms.
    #[test]
    fn la_tolerancia_incluye_su_extremo() {
        let opts = CompareOptions::cheap();
        assert_eq!(
            decide(&file(10, 0), &file(10, 2_000), &opts, &no_hash()).verdict,
            CompareVerdict::Same
        );
        assert_eq!(
            decide(&file(10, 0), &file(10, 2_001), &opts, &no_hash()).verdict,
            CompareVerdict::Different
        );
    }

    /// La cascada es SIMÉTRICA: cambiar los lados de sitio cambia el lado de
    /// `newer` y nada más. El walk apoya su test de espejo en esto.
    #[test]
    fn cambiar_los_lados_de_sitio_solo_cambia_el_lado_mas_nuevo() {
        let opts = CompareOptions::cheap();
        let parejas = [
            (file(10, 0), file(20, 0)),
            (file(10, 0), file(10, 9_000)),
            (file(10, 0), file(10, None)),
            (file(None, 0), file(10, 0)),
            (file(10, 0), dir()),
            (link(), link()),
        ];
        for (l, r) in parejas {
            let ida = decide(&l, &r, &opts, &targets(b"../a", b"../b"));
            let vuelta = decide(&r, &l, &opts, &targets(b"../b", b"../a"));
            assert_eq!(
                (ida.verdict, ida.criterion, ida.confidence),
                (vuelta.verdict, vuelta.criterion, vuelta.confidence),
                "{l:?} vs {r:?}"
            );
            let espejo = match vuelta.newer {
                Some(Side::Left) => Some(Side::Right),
                Some(Side::Right) => Some(Side::Left),
                otro => otro,
            };
            assert_eq!(ida.newer, espejo, "{l:?} vs {r:?}");
        }
    }

    /// El rung caro NO decide aquí: `decide` es síncrona y no lee contenido.
    /// Cuando los baratos dan la pareja por igual y el llamante pidió hash, la
    /// decisión sale marcada como NO final, y el walk vuelve con el resultado.
    #[test]
    fn el_rung_de_hash_se_pide_y_luego_se_contesta() {
        let opts = with_hash();

        let pendiente = decide(&file(10, 0), &file(10, 0), &opts, &no_hash());
        assert!(pendiente.needs_hash, "los baratos dijeron `Same`");
        assert_eq!(pendiente.criterion, CompareCriterion::Mtime);

        let iguales = decide(
            &file(10, 0),
            &file(10, 0),
            &opts,
            &no_hash().with_hash(HashOutcome::Equal),
        );
        assert_eq!(
            (iguales.verdict, iguales.criterion, iguales.confidence),
            (
                CompareVerdict::Same,
                CompareCriterion::Hash,
                CompareConfidence::Certain
            )
        );
        assert!(!iguales.needs_hash);

        let distintos = decide(
            &file(10, 0),
            &file(10, 0),
            &opts,
            &no_hash().with_hash(HashOutcome::Differ),
        );
        assert_eq!(
            (distintos.verdict, distintos.criterion, distintos.confidence),
            (
                CompareVerdict::Different,
                CompareCriterion::Hash,
                CompareConfidence::Certain
            )
        );
    }

    /// El rung caro alcanza SOLO a lo que los baratos dieron por igual, y solo
    /// a ficheros. Hashear una pareja que ya se sabe distinta es leer dos
    /// ficheros enteros para no aprender nada.
    #[test]
    fn el_hash_no_alcanza_a_lo_que_ya_esta_decidido() {
        let opts = with_hash();
        for (l, r) in [
            (file(10, 0), file(20, 0)),     // distinto por tamaño
            (file(10, 0), file(10, 9_000)), // distinto por fecha
            (file(10, 0), dir()),           // distinto por kind
            (dir(), dir()),                 // un directorio no tiene contenido
            (link(), link()),               // ya decidió el destino
        ] {
            let d = decide(&l, &r, &opts, &targets(b"../a", b"../a"));
            assert!(!d.needs_hash, "{l:?} vs {r:?}");
        }
    }

    /// Un tamaño desconocido tampoco impide hashear: es justo la pareja que el
    /// rung caro convierte de `Unknown` en `Certain`.
    #[test]
    fn una_pareja_sin_tamano_tambien_se_puede_verificar() {
        let d = decide(&file(None, 0), &file(10, 0), &with_hash(), &no_hash());
        assert!(d.needs_hash);
        assert_eq!(d.confidence, CompareConfidence::Unknown);
    }

    /// Apagar un rung lo SALTA, no lo convierte en `Different`.
    #[test]
    fn apagar_un_rung_lo_salta() {
        let sin_tamano = CompareOptions {
            criteria: CompareCriteria {
                size: false,
                ..CompareCriteria::default()
            },
            ..CompareOptions::cheap()
        };
        let d = decide(&file(10, 0), &file(999, 500), &sin_tamano, &no_hash());
        assert_eq!(
            (d.verdict, d.criterion, d.confidence),
            (
                CompareVerdict::Same,
                CompareCriterion::Mtime,
                CompareConfidence::Probable
            ),
            "sin el rung de tamaño decide la fecha"
        );

        // Sin NINGÚN rung no hay criterio que haya decidido: `Presence` por
        // convención del wire, y `Unknown` porque no se comparó nada.
        let sin_nada = CompareOptions {
            criteria: CompareCriteria {
                size: false,
                mtime: false,
                hash: false,
            },
            ..CompareOptions::cheap()
        };
        let d = decide(&file(10, 0), &file(999, 500), &sin_nada, &no_hash());
        assert_eq!(
            (d.verdict, d.criterion, d.confidence),
            (
                CompareVerdict::Same,
                CompareCriterion::Presence,
                CompareConfidence::Unknown
            )
        );
    }

    /// El rung de presencia: el más barato y el único que no compara nada.
    #[test]
    fn la_presencia_decide_con_certeza_y_produce_una_fila_consistente() {
        let izq = Decision::only_left();
        assert_eq!(
            (izq.verdict, izq.criterion, izq.confidence),
            (
                CompareVerdict::OnlyLeft,
                CompareCriterion::Presence,
                CompareConfidence::Certain
            )
        );
        let fila = izq.into_row(3, Some(file(10, 0)), None);
        assert!(fila.sides_are_consistent());
        assert!(fila.reason_is_consistent());
        assert_eq!(fila.id, 3);

        let der = Decision::only_right();
        assert_eq!(der.verdict, CompareVerdict::OnlyRight);
        assert!(
            der.into_row(4, None, Some(file(10, 0)))
                .sides_are_consistent()
        );
    }

    /// Una decisión de la cascada se convierte en fila SIN perder nada: el lado
    /// más nuevo viaja, y ni el motivo ni el lado se inventan.
    #[test]
    fn la_decision_viaja_entera_a_la_fila() {
        let d = decide(
            &file(10, 0),
            &file(10, 9_000),
            &CompareOptions::cheap(),
            &no_hash(),
        );
        let fila = d.into_row(9, Some(file(10, 0)), Some(file(10, 9_000)));
        assert_eq!(fila.verdict, CompareVerdict::Different);
        assert_eq!(fila.criterion, CompareCriterion::Mtime);
        assert_eq!(fila.confidence, CompareConfidence::Probable);
        assert_eq!(fila.newer, Some(Side::Right));
        assert_eq!(fila.reason, None);
        assert_eq!(fila.side, None);
        assert!(fila.sides_are_consistent() && fila.reason_is_consistent());
    }
}
