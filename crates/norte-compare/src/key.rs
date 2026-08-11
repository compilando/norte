//! La clave de emparejamiento: qué nombre de un lado se mide contra qué nombre
//! del otro, y qué dos nombres de UN MISMO lado colapsan en uno.
//!
//! La clave existe SOLO para emparejar. Jamás se pinta, jamás se opera con
//! ella, jamás sustituye a los bytes del nombre: cada [`Entry`] viaja en su
//! fila con sus bytes originales intactos (regla dura 1). Aquí no hay ni una
//! conversión con pérdida — un nombre que no es UTF-8 no es texto, y se
//! empareja por sus bytes.
//!
//! Dos transformaciones, en este orden:
//!
//! 1. **Plegado de caja**, cuando *alguno* de los dos lados no declara
//!    [`CapabilityFlags::CASE_SENSITIVE`]. Un lado que no distingue caja no
//!    puede tener a la vez `README` y `readme`, así que emparejar CONTRA él es
//!    plegar — aunque el otro lado sea ext4.
//! 2. **NFC**, cuando los bytes son UTF-8 válido. macOS reparte NFD y Linux
//!    NFC; el mismo fichero copiado entre los dos tiene que emparejar.
//!
//! El orden importa poco pero es fijo: plegar y DESPUÉS normalizar es una sola
//! pasada de NFC y deja `É` (NFC) y `E`+`◌́` (NFD) en la misma clave por los dos
//! caminos.

use std::borrow::Cow;
use std::collections::BTreeMap;

use norte_proto::Segment;
use norte_proto::methods::CompareReason;
use norte_vfs::{Capabilities, CapabilityFlags, Entry};
use unicode_normalization::{UnicodeNormalization, is_nfc};

/// Cómo empareja LA PAREJA de lados, que no es lo mismo que cómo es cada uno.
///
/// El plegado de caja es propiedad del par y no de un lado: basta con que uno
/// de los dos no distinga caja para que la comparación entera tenga que
/// plegar, porque ese lado no puede sostener las dos grafías.
///
/// ```
/// use norte_compare::Sides;
/// use norte_vfs::{Capabilities, CapabilityFlags};
///
/// let ext4 = Capabilities { flags: CapabilityFlags::CASE_SENSITIVE, max_path: None };
/// let apfs = Capabilities { flags: CapabilityFlags::CASE_PRESERVING, max_path: None };
/// assert!(!Sides::from_capabilities(ext4, ext4).folds_case());
/// assert!(Sides::from_capabilities(ext4, apfs).folds_case(), "basta con UN lado");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Sides {
    fold_case: bool,
}

impl Sides {
    /// A partir de si cada lado distingue caja.
    ///
    /// ```
    /// use norte_compare::Sides;
    /// assert!(Sides::new(true, false).folds_case());
    /// assert!(!Sides::new(true, true).folds_case());
    /// ```
    #[must_use]
    pub fn new(left_case_sensitive: bool, right_case_sensitive: bool) -> Self {
        Self {
            fold_case: !(left_case_sensitive && right_case_sensitive),
        }
    }

    /// A partir de las [`Capabilities`] que declaran los dos providers.
    #[must_use]
    pub fn from_capabilities(left: Capabilities, right: Capabilities) -> Self {
        Self::new(
            left.flags.contains(CapabilityFlags::CASE_SENSITIVE),
            right.flags.contains(CapabilityFlags::CASE_SENSITIVE),
        )
    }

    /// Los dos lados distinguen caja (ext4 contra ext4): NO se pliega.
    #[must_use]
    pub fn both_case_sensitive() -> Self {
        Self::new(true, true)
    }

    /// El lado izquierdo no distingue caja: se pliega.
    #[must_use]
    pub fn left_case_insensitive() -> Self {
        Self::new(false, true)
    }

    /// El lado derecho no distingue caja: se pliega.
    #[must_use]
    pub fn right_case_insensitive() -> Self {
        Self::new(true, false)
    }

    /// ¿Pliega caja este emparejamiento?
    #[must_use]
    pub fn folds_case(self) -> bool {
        self.fold_case
    }
}

/// La clave por la que dos nombres emparejan.
///
/// Presta los bytes del nombre mientras la transformación no cambia nada — el
/// caso abrumadoramente común (ASCII en minúsculas, ya-NFC, no-UTF8) — y solo
/// materializa cuando sí cambia.
///
/// `PartialEq`/`Ord`/`Hash` van por los BYTES, así que una clave prestada y una
/// propia con el mismo contenido son la misma clave.
///
/// ```
/// use norte_compare::{Sides, key_for};
/// let k = key_for(b"README", Sides::right_case_insensitive());
/// assert_eq!(k.as_bytes(), b"readme");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PairKey<'a>(Cow<'a, [u8]>);

impl PairKey<'_> {
    /// Los bytes de la clave. NO son el nombre: no se pintan ni se operan.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// Suelta el préstamo del nombre, copiando si hacía falta.
    #[must_use]
    pub fn into_owned(self) -> PairKey<'static> {
        PairKey(Cow::Owned(self.0.into_owned()))
    }
}

/// ¿Cambia algo pasar `s` a minúsculas? Sin alocar: compara el iterador de
/// caracteres plegados contra el original. Cubre las expansiones de más de un
/// carácter (`İ` → `i`+`◌̇`) y los titlecase (`ǅ` → `ǆ`), que
/// `char::is_uppercase` no ve.
fn lowercasing_changes(s: &str) -> bool {
    !s.chars().flat_map(char::to_lowercase).eq(s.chars())
}

/// La clave de emparejamiento de un nombre bajo unos [`Sides`].
///
/// Los bytes de entrada no se tocan: lo que sale es una clave, y el nombre
/// sigue siendo el nombre.
///
/// Un nombre que NO es UTF-8 no se normaliza (no hay texto que normalizar),
/// pero sí se pliega en ASCII cuando el par pliega: un volumen que no distingue
/// caja tampoco puede sostener `README\xff` y `readme\xff` a la vez, y el
/// motivo por el que no puede no depende de que el resto de los bytes sean
/// texto.
///
/// ```
/// use norte_compare::{Sides, key_for};
/// // NFD y NFC del mismo nombre emparejan...
/// let sensible = Sides::both_case_sensitive();
/// assert_eq!(key_for("café".as_bytes(), sensible), key_for(b"cafe\xcc\x81", sensible));
/// // ...y los bytes que no son texto pasan tal cual.
/// assert_eq!(key_for(b"roto\xff\xfe", sensible).as_bytes(), b"roto\xff\xfe");
/// ```
#[must_use]
pub fn key_for(name: &[u8], sides: Sides) -> PairKey<'_> {
    let Ok(text) = std::str::from_utf8(name) else {
        // No es texto: ni NFC ni plegado Unicode. Solo ASCII, y solo si hace
        // falta.
        return if sides.fold_case && name.iter().any(u8::is_ascii_uppercase) {
            PairKey(Cow::Owned(name.to_ascii_lowercase()))
        } else {
            PairKey(Cow::Borrowed(name))
        };
    };

    let folded: Cow<'_, str> = if sides.fold_case && lowercasing_changes(text) {
        Cow::Owned(text.to_lowercase())
    } else {
        Cow::Borrowed(text)
    };

    if is_nfc(&folded) {
        return match folded {
            Cow::Borrowed(same) => PairKey(Cow::Borrowed(same.as_bytes())),
            Cow::Owned(owned) => PairKey(Cow::Owned(owned.into_bytes())),
        };
    }
    PairKey(Cow::Owned(folded.nfc().collect::<String>().into_bytes()))
}

/// Lo que [`index_side`] necesita de una entrada: los BYTES de su nombre.
///
/// Existe para que los tests del emparejamiento puedan hablar de nombres
/// sueltos (`&[u8]`) y el walk de [`Entry`], sin dos copias de la misma
/// lógica.
pub trait PairName {
    /// Los bytes del nombre, tal y como los dio el provider.
    fn pair_name(&self) -> &[u8];
}

impl PairName for &[u8] {
    fn pair_name(&self) -> &[u8] {
        self
    }
}

impl PairName for Vec<u8> {
    fn pair_name(&self) -> &[u8] {
        self
    }
}

impl PairName for Entry {
    /// El último segmento del `VPath`, en bytes. La raíz — que no tiene
    /// nombre — empareja por el nombre vacío; el walk nunca la mete en un
    /// listado.
    fn pair_name(&self) -> &[u8] {
        self.path.file_name().map_or(&[][..], Segment::as_bytes)
    }
}

/// Un lado listo para emparejar: las entradas indexadas por su clave, y qué
/// entradas colisionan con cuáles.
///
/// Presta el listado; no lo copia ni lo reordena.
#[derive(Debug, Clone)]
pub struct SideIndex<'a, T> {
    entries: &'a [T],
    /// Clave → índices en `entries`, en orden de listado. `BTreeMap` porque el
    /// merge-join quiere las claves ORDENADAS y el listado de un provider no
    /// garantiza orden alguno.
    by_key: BTreeMap<PairKey<'a>, Vec<usize>>,
    /// Motivo por entrada. `None` = esta entrada no colisiona con ninguna.
    reasons: Vec<Option<CompareReason>>,
}

/// Indexa UN lado por su clave de emparejamiento.
///
/// Las entradas que colisionan **no se emparejan y no se deduplican jamás**:
/// se conservan TODAS, una por una, porque cada una es un fichero real sobre el
/// que una sincronización posterior podría escribir. Perder un nombre aquí es
/// perder exactamente ese fichero. Salen por [`SideIndex::collisions`], una por
/// entrada, que es la forma normativa de una fila
/// [`CompareVerdict::Ambiguous`](norte_proto::methods::CompareVerdict::Ambiguous):
/// una fila por entrada implicada, el otro lado en `None`.
///
/// ```
/// use norte_compare::{CompareReason, Sides, index_side};
/// let listado: Vec<&[u8]> = vec![b"README", b"readme", b"NOTES"];
/// let lado = index_side(&listado, Sides::right_case_insensitive());
///
/// // Dos colisiones: DOS filas, ninguna deduplicada.
/// let chocan: Vec<_> = lado.collisions().collect();
/// assert_eq!(chocan.len(), 2);
/// assert!(chocan.iter().all(|(_, r)| *r == CompareReason::CaseFold));
///
/// // Y lo que no colisiona sí empareja.
/// assert_eq!(lado.unique().count(), 1);
/// ```
#[must_use]
pub fn index_side<T: PairName>(entries: &[T], sides: Sides) -> SideIndex<'_, T> {
    let mut by_key: BTreeMap<PairKey<'_>, Vec<usize>> = BTreeMap::new();
    for (i, entry) in entries.iter().enumerate() {
        by_key
            .entry(key_for(entry.pair_name(), sides))
            .or_default()
            .push(i);
    }

    let mut reasons = vec![None; entries.len()];
    // El motivo de CADA entrada colisionada: ¿la colisión sobrevive sin
    // plegar? Entonces la causó normalizar. ¿Se deshace al no plegar? Entonces
    // la causó el plegado. Se calcula por entrada y no por grupo porque un
    // grupo de tres puede tener una causa distinta para cada par.
    let sin_plegar = Sides::both_case_sensitive();
    for idxs in by_key.values() {
        if idxs.len() < 2 {
            continue;
        }
        let crudas: Vec<PairKey<'_>> = idxs
            .iter()
            .map(|&i| key_for(entries[i].pair_name(), sin_plegar))
            .collect();
        for (pos, &i) in idxs.iter().enumerate() {
            let gemela = crudas
                .iter()
                .enumerate()
                .any(|(otra, k)| otra != pos && *k == crudas[pos]);
            reasons[i] = Some(if gemela {
                CompareReason::Normalization
            } else {
                CompareReason::CaseFold
            });
        }
    }

    SideIndex {
        entries,
        by_key,
        reasons,
    }
}

impl<'a, T: PairName> SideIndex<'a, T> {
    /// El listado que se indexó, en su orden original.
    #[must_use]
    pub fn entries(&self) -> &'a [T] {
        self.entries
    }

    /// Cuántas entradas trae el listado (colisionadas incluidas).
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// ¿Listado vacío?
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Las entradas EMPAREJABLES, por clave y en orden de clave: las que no
    /// comparten la suya con ninguna otra de su lado.
    ///
    /// Es la mitad del merge-join. Lo que falta —lo colisionado— sale por
    /// [`SideIndex::collisions`] y no empareja con nada.
    pub fn unique(&self) -> impl Iterator<Item = (&PairKey<'a>, &'a T)> {
        self.by_key.iter().filter_map(|(k, idxs)| match idxs[..] {
            [i] => Some((k, &self.entries[i])),
            _ => None,
        })
    }

    /// Busca una entrada emparejable por su clave. `None` si no está o si su
    /// clave colisiona (una clave ambigua NO empareja).
    #[must_use]
    pub fn get(&self, key: &PairKey<'a>) -> Option<&'a T> {
        match self.by_key.get(key)?[..] {
            [i] => Some(&self.entries[i]),
            _ => None,
        }
    }

    /// TODAS las entradas colisionadas, en orden de listado, con su motivo.
    /// Una por entrada: dos nombres que colapsan son DOS, jamás una fusión y
    /// jamás una deduplicación.
    pub fn collisions(&self) -> impl Iterator<Item = (&'a T, CompareReason)> {
        self.entries
            .iter()
            .zip(self.reasons.iter())
            .filter_map(|(e, r)| r.map(|r| (e, r)))
    }

    /// Por qué colisiona la entrada cuyos bytes de nombre son `name`, o `None`
    /// si no colisiona (o si no está en el listado).
    ///
    /// Recorre el listado: es una consulta de test y de diagnóstico. El walk
    /// usa [`SideIndex::collisions`], que va en una pasada.
    #[must_use]
    pub fn ambiguous_reason(&self, name: &[u8]) -> Option<CompareReason> {
        let i = self.entries.iter().position(|e| e.pair_name() == name)?;
        self.reasons[i]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// macOS hands out NFD, Linux NFC. The same file copied between them must
    /// pair, and BOTH original byte strings must survive for display — the key
    /// is for pairing and nothing else (rule 1).
    #[test]
    fn nfd_and_nfc_of_one_name_share_a_key() {
        let nfc = "café".as_bytes(); // e-acute as one code point
        let nfd = b"cafe\xcc\x81"; // e + combining acute
        let sensitive = Sides::both_case_sensitive();
        assert_eq!(key_for(nfc, sensitive), key_for(nfd, sensitive));
    }

    /// Bytes that are not UTF-8 are not text, cannot be normalised, and must
    /// pass through untouched rather than through a lossy conversion.
    #[test]
    fn non_utf8_names_pass_through_raw() {
        let raw = b"broken\xff\xfename";
        assert_eq!(key_for(raw, Sides::both_case_sensitive()).as_bytes(), raw);
    }

    /// Case folding is decided by the PAIR, not by one side: a
    /// case-insensitive side cannot hold both spellings, so pairing against it
    /// must fold even when the other side is ext4.
    #[test]
    fn one_case_insensitive_side_folds_the_pairing() {
        let both = Sides::both_case_sensitive();
        assert_ne!(key_for(b"README", both), key_for(b"readme", both));
        let mixed = Sides::right_case_insensitive();
        assert_eq!(key_for(b"README", mixed), key_for(b"readme", mixed));
    }

    /// Two entries on ONE side collapsing to one key is the collision a later
    /// synchronisation has to see BEFORE it writes. They are reported, never
    /// paired, and never silently deduplicated.
    #[test]
    fn same_side_collision_is_reported_with_its_reason() {
        let names: Vec<&[u8]> = vec![b"README", b"readme", b"NOTES"];
        let folded = index_side(&names, Sides::right_case_insensitive());
        assert_eq!(
            folded.ambiguous_reason(b"README"),
            Some(CompareReason::CaseFold)
        );
        assert_eq!(
            folded.ambiguous_reason(b"readme"),
            Some(CompareReason::CaseFold)
        );
        assert_eq!(folded.ambiguous_reason(b"NOTES"), None);

        let nfd: Vec<&[u8]> = vec!["café".as_bytes(), b"cafe\xcc\x81"];
        let normalised = index_side(&nfd, Sides::both_case_sensitive());
        assert_eq!(
            normalised.ambiguous_reason("café".as_bytes()),
            Some(CompareReason::Normalization)
        );
    }

    // ---- lo que los cuatro de arriba no fijan ----

    /// La clave no muta los bytes: los presta cuando puede y los copia cuando
    /// no, pero el nombre de entrada sigue siendo el que era (regla 1). Esto
    /// es lo que hace legítimo emparejar por clave y pintar por bytes.
    #[test]
    fn la_clave_no_toca_los_bytes_del_nombre() {
        let nombre = b"CAFE\xcc\x81\xff";
        let clave = key_for(nombre, Sides::right_case_insensitive());
        assert_ne!(
            clave.as_bytes(),
            nombre,
            "no-UTF8 con mayúsculas: se pliega"
        );
        assert_eq!(clave.as_bytes(), b"cafe\xcc\x81\xff");
        assert_eq!(nombre, b"CAFE\xcc\x81\xff", "el nombre no se ha tocado");
    }

    /// Un nombre que no es texto tampoco puede vivir dos veces en un volumen
    /// que no distingue caja: el plegado ASCII también le toca.
    #[test]
    fn los_bytes_no_utf8_tambien_pliegan_en_ascii() {
        let mixto = Sides::left_case_insensitive();
        assert_eq!(key_for(b"ROTO\xff", mixto), key_for(b"roto\xff", mixto));
        let sensible = Sides::both_case_sensitive();
        assert_ne!(
            key_for(b"ROTO\xff", sensible),
            key_for(b"roto\xff", sensible)
        );
    }

    /// Plegar y normalizar CONMUTAN en la clave: `É` (NFC) y `E`+`◌́` (NFD)
    /// llegan al mismo sitio, plegando o sin plegar.
    #[test]
    fn plegado_y_nfc_se_componen_en_los_dos_ordenes() {
        let mixto = Sides::right_case_insensitive();
        let nfc_mayus = "CAFÉ".as_bytes();
        let nfd_minus = b"cafe\xcc\x81";
        assert_eq!(key_for(nfc_mayus, mixto), key_for(nfd_minus, mixto));
        // Sin plegar NO emparejan: la mayúscula es una diferencia real en ext4.
        let sensible = Sides::both_case_sensitive();
        assert_ne!(key_for(nfc_mayus, sensible), key_for(nfd_minus, sensible));
    }

    /// El plegado Unicode no se queda en ASCII.
    #[test]
    fn el_plegado_cubre_mas_que_ascii() {
        let mixto = Sides::left_case_insensitive();
        assert_eq!(
            key_for("AÑO".as_bytes(), mixto),
            key_for("año".as_bytes(), mixto)
        );
        // Titlecase: `char::is_uppercase` diría que no, y sí pliega.
        assert_eq!(
            key_for("ǅ".as_bytes(), mixto),
            key_for("ǆ".as_bytes(), mixto)
        );
    }

    /// Una entrada colisionada NO empareja: ni por `unique`, ni por `get`.
    /// Emparejarla sería elegir a ciegas cuál de los dos ficheros es «el» par.
    #[test]
    fn una_clave_colisionada_no_empareja_con_nadie() {
        let names: Vec<&[u8]> = vec![b"README", b"readme", b"NOTES"];
        let lado = index_side(&names, Sides::right_case_insensitive());

        let emparejables: Vec<&[u8]> = lado.unique().map(|(_, e)| *e).collect();
        assert_eq!(emparejables, vec![&b"NOTES"[..]]);
        assert!(
            lado.get(&key_for(b"readme", Sides::both_case_sensitive()))
                .is_none()
        );
        assert!(
            lado.get(&key_for(b"NOTES", Sides::right_case_insensitive()))
                .is_some()
        );
    }

    /// Tres nombres que colapsan son TRES filas. Ninguna se funde y ninguna se
    /// pierde: cada una es un fichero sobre el que la spec 2 podría escribir.
    #[test]
    fn cada_entrada_colisionada_sale_una_vez() {
        let names: Vec<&[u8]> = vec![b"A", b"a", b"A", b"b"];
        let lado = index_side(&names, Sides::right_case_insensitive());
        let chocan: Vec<&[u8]> = lado.collisions().map(|(e, _)| *e).collect();
        assert_eq!(chocan, vec![&b"A"[..], &b"a"[..], &b"A"[..]]);
        assert_eq!(lado.unique().count(), 1, "solo `b` empareja");
        assert_eq!(lado.len(), 4, "el listado no se ha deduplicado");
    }

    /// El motivo es por ENTRADA, no por grupo: en un grupo mixto, quien tiene
    /// gemelo por normalización dice `Normalization` y quien solo colapsó al
    /// plegar dice `CaseFold`.
    #[test]
    fn el_motivo_lo_da_la_transformacion_que_colapso_esa_entrada() {
        let compuesto = "café".as_bytes();
        let descompuesto = b"cafe\xcc\x81";
        let mayusculas = "CAFÉ".as_bytes();
        let names: Vec<&[u8]> = vec![compuesto, descompuesto, mayusculas];
        let lado = index_side(&names, Sides::right_case_insensitive());

        assert_eq!(
            lado.ambiguous_reason(compuesto),
            Some(CompareReason::Normalization),
            "tiene gemelo sin plegar"
        );
        assert_eq!(
            lado.ambiguous_reason(descompuesto),
            Some(CompareReason::Normalization)
        );
        assert_eq!(
            lado.ambiguous_reason(mayusculas),
            Some(CompareReason::CaseFold),
            "sin plegar no chocaba con nadie"
        );
    }

    /// Sin plegado no hay `CaseFold`: dos grafías distintas son dos ficheros
    /// distintos y emparejan cada uno por su lado.
    #[test]
    fn sin_plegado_las_dos_grafias_son_dos_entradas() {
        let names: Vec<&[u8]> = vec![b"README", b"readme"];
        let lado = index_side(&names, Sides::both_case_sensitive());
        assert_eq!(lado.collisions().count(), 0);
        assert_eq!(lado.unique().count(), 2);
    }

    /// Un listado vacío no colisiona ni empareja, y no revienta.
    #[test]
    fn un_lado_vacio_es_un_lado() {
        let names: Vec<&[u8]> = vec![];
        let lado = index_side(&names, Sides::both_case_sensitive());
        assert!(lado.is_empty());
        assert_eq!(lado.unique().count(), 0);
        assert_eq!(lado.collisions().count(), 0);
        assert_eq!(lado.ambiguous_reason(b"nada"), None);
    }

    /// `Entry` empareja por el ÚLTIMO segmento de su `VPath`, con sus bytes
    /// crudos: es lo que el walk le va a pasar.
    #[test]
    fn una_entry_empareja_por_los_bytes_de_su_ultimo_segmento() {
        use norte_proto::{EntryKind, VPath};

        let entry = |wire: &str| Entry {
            path: VPath::parse(wire).expect("path"),
            kind: EntryKind::File,
            size: None,
            mtime_ms: None,
            attrs: std::collections::BTreeMap::default(),
        };
        // `informe\xff.dat` percent-encoded: los bytes vuelven exactos.
        let crudo = entry("file:///a/informe%FF.dat");
        assert_eq!(crudo.pair_name(), b"informe\xff.dat");

        let nfd = entry("file:///a/cafe%CC%81");
        let nfc = entry("file:///b/caf%C3%A9");
        let sensible = Sides::both_case_sensitive();
        assert_eq!(
            key_for(nfd.pair_name(), sensible),
            key_for(nfc.pair_name(), sensible)
        );
        assert_ne!(
            nfd.pair_name(),
            nfc.pair_name(),
            "los bytes siguen siendo dos"
        );
    }

    /// El orden de las claves es el del merge-join, y no el del listado (que
    /// no garantiza ninguno).
    #[test]
    fn las_claves_salen_ordenadas() {
        let names: Vec<&[u8]> = vec![b"zeta", b"alfa", b"Mu"];
        let lado = index_side(&names, Sides::right_case_insensitive());
        let claves: Vec<Vec<u8>> = lado.unique().map(|(k, _)| k.as_bytes().to_vec()).collect();
        assert_eq!(
            claves,
            vec![b"alfa".to_vec(), b"mu".to_vec(), b"zeta".to_vec()]
        );
    }

    /// Una clave prestada y una copiada con los mismos bytes son LA MISMA
    /// clave: si no, un lado que normalizó no encontraría al otro que no.
    #[test]
    fn prestada_y_propia_son_la_misma_clave() {
        let sensible = Sides::both_case_sensitive();
        let prestada = key_for(b"cafe", sensible);
        let propia = key_for(b"cafe\xcc\x81", sensible).into_owned();
        assert_ne!(prestada, propia);
        assert_eq!(prestada.clone().into_owned(), prestada);
        assert_eq!(propia.as_bytes(), "café".as_bytes());
    }
}
