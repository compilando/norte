//! La clave de emparejamiento: qué nombre de un lado se mide contra qué nombre
//! del otro, y qué dos nombres de UN MISMO lado colapsan en uno.
//!
//! La clave existe SOLO para emparejar. Jamás se pinta, jamás se opera con
//! ella, jamás sustituye a los bytes del nombre: cada [`Entry`] viaja en su
//! fila con sus bytes originales intactos (regla dura 1). Aquí no hay ni una
//! conversión con pérdida — un nombre que no es UTF-8 no es texto, y se
//! empareja por sus bytes.
//!
//! El propio plegado — el delta de `fold_delta`, el orden plegar-y-DESPUÉS-
//! normalizar, y qué pasa con un nombre que no es UTF-8 — vive en
//! [`norte_encoding::name_key`] (ADR 0051, #151): este módulo era una segunda
//! copia de esa función, y la copia llegó a divertir una vez (`key_for` envió
//! la clave pre-#129 durante todo un ciclo de release, sin nada que lo
//! comparase). Lo que este módulo aporta por encima es lo que es DE la
//! comparación y no del texto: [`Sides`] decide si la pareja pliega a partir
//! de las [`Capabilities`] de los dos lados, y [`SideIndex`] indexa un
//! listado por su clave y separa lo que empareja de lo que colisiona.
use std::borrow::Cow;
use std::collections::BTreeMap;

use norte_encoding::FoldMode;
use norte_proto::Segment;
use norte_proto::methods::CompareReason;
use norte_vfs::{Capabilities, CapabilityFlags, Entry};

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

/// La clave de emparejamiento de un nombre bajo unos [`Sides`].
///
/// Delega en [`norte_encoding::name_key`] (ADR 0051, #151): esta función
/// SOLO traduce `sides.fold_case` a un [`FoldMode`] — nunca
/// [`FoldMode::Full`], que existe para ext4/f2fs `+F` (#145) pero nada de
/// este motor sabe hoy si un directorio lo tiene, y encenderlo a ciegas sería
/// peor que el hueco que cerraría.
///
/// Los bytes de entrada no se tocan: lo que sale es una clave, y el nombre
/// sigue siendo el nombre. Un nombre que NO es UTF-8 se empareja por sus
/// bytes; ver el rustdoc de [`norte_encoding::name_key`] para qué pasa con un
/// byte inválido que no es TODO el nombre (#154) y por qué un byte de cola
/// Shift-JIS nunca se pliega como si fuera ASCII.
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
    let mode = if sides.fold_case {
        FoldMode::Simple
    } else {
        FoldMode::None
    };
    PairKey(norte_encoding::name_key(name, mode))
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

    /// Los bytes de una fixture del corpus canónico de `norte-testkit`.
    ///
    /// Los nombres hostiles se toman de ahí y no se escriben a mano: la mitad
    /// de este módulo prueba cosas que solo se ven con el byte exacto, y el
    /// corpus ya trae —con su porqué escrito— las parejas que costaron el
    /// #129.
    fn corpus(id: &str) -> Vec<u8> {
        norte_testkit::corpus::hostile_names()
            .into_iter()
            .find(|n| n.id == id)
            .unwrap_or_else(|| panic!("fixture {id} en el corpus"))
            .bytes
    }

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

    /// La clave nunca muta los bytes de ENTRADA (regla 1): `key_for` presta o
    /// copia para construir la clave, pero `nombre` sigue siendo el que era.
    /// Esto es lo que hace legítimo emparejar por clave y pintar por bytes.
    ///
    /// La CLAVE en sí, en cambio, sí pliega y normaliza el prefijo válido —
    /// `CAFE`+U+0301 es texto UTF-8 de verdad, y el `\xff` que sigue no lo
    /// invalida (#154): antes de la corrección, un solo byte roto al final
    /// desactivaba el plegado del resto entero.
    #[test]
    fn la_clave_no_toca_los_bytes_del_nombre() {
        let nombre = b"CAFE\xcc\x81\xff";
        let clave = key_for(nombre, Sides::right_case_insensitive());
        assert_eq!(
            clave.as_bytes(),
            "café"
                .as_bytes()
                .iter()
                .chain(b"\xff")
                .copied()
                .collect::<Vec<u8>>(),
            "el prefijo válido pliega y normaliza; el byte roto pasa tal cual",
        );
        assert_eq!(
            nombre, b"CAFE\xcc\x81\xff",
            "el nombre de ENTRADA no se ha tocado"
        );
    }

    /// Un nombre que no es UTF-8 EN NINGÚN PREFIJO no se pliega, ni siquiera
    /// en ASCII — `ROTO\xff` sí pliega desde #154, porque `ROTO` es un
    /// prefijo válido; ver `la_clave_no_toca_los_bytes_del_nombre` para esa
    /// mitad. Esta prueba es la otra: cuando NO hay ni un byte de prefijo
    /// válido, plegar sería el bug que #129 cerró.
    ///
    /// Parece inofensivo y no lo es: en los encodings legacy de doble byte el
    /// byte de cola cae donde viven `A`–`Z`. `shift_jis_tesuto` (テスト) es
    /// `83 65 83 58 83 67` y ese `58` es una `X`; plegarlo convierte ス en ベ,
    /// que es otro carácter. Y `norte_core::rename::plan::name_key` tampoco lo
    /// pliega: las dos respuestas a «¿colisionan estos dos nombres?» tienen que
    /// ser la misma (#151).
    #[test]
    fn los_bytes_no_utf8_no_se_pliegan() {
        let mixto = Sides::left_case_insensitive();
        // Prefijo válido: pliega. Ver #154 — un byte roto al final ya no
        // desactiva el plegado del texto que sí lo es.
        assert_eq!(key_for(b"ROTO\xff", mixto), key_for(b"roto\xff", mixto));

        let tesuto = corpus("shift_jis_tesuto");
        assert_eq!(
            key_for(&tesuto, mixto).as_bytes(),
            &tesuto[..],
            "el byte de cola `58` de ス es una `X` en ASCII"
        );

        // Y el par que lo demuestra de verdad: ア y ヂ solo se diferencian en
        // su byte de cola, `41` contra `61`. Plegar los declararía el mismo
        // fichero.
        assert_ne!(key_for(b"\x83\x41", mixto), key_for(b"\x83\x61", mixto));
    }

    /// El plegado es plegado de CAJA, no `to_lowercase`, y el corpus ya traía
    /// las parejas que lo distinguen (#129, revisión de C2–C5).
    ///
    /// Cada pareja es UN fichero en APFS, NTFS y en un directorio ext4 `+F`.
    /// Con `to_lowercase` a secas salían dos claves — o sea, dos filas
    /// `OnlyLeft`/`OnlyRight` que un plan de sincronización copiaría una encima
    /// de la otra.
    #[test]
    fn el_plegado_es_case_folding_y_no_el_mapeo_a_minusculas() {
        let mixto = Sides::right_case_insensitive();
        for (izq, der) in [
            // ΟΔΟΣ / οδοσ: `str::to_lowercase` aplica Final_Sigma y produce ς.
            ("greek_uppercase_final_sigma", "greek_medial_sigma_twin"),
            // µm.txt (U+00B5) / μm.txt (U+03BC): Unicode ya llama minúscula al
            // signo micro, así que `to_lowercase` no lo mueve.
            ("micro_sign_mu", "greek_mu_twin"),
            // ﬅ.txt / ﬆ.txt: la única ligadura con pliegue simple.
            ("ligature_long_st", "ligature_st"),
            // J+◌̌ / ǰ: plegar RECOMPONE, así que el NFC va después.
            (
                "nfd_uppercase_composed_only_lowercase",
                "precomposed_lowercase_j_caron",
            ),
        ] {
            let (a, b) = (corpus(izq), corpus(der));
            assert_eq!(key_for(&a, mixto), key_for(&b, mixto), "{izq}");
        }

        // Y el hueco ACEPTADO, que sigue siéndolo: `ß` solo tiene pliegue
        // COMPLETO (a `ss`), que expande, y esta clave es `char → char`. En
        // APFS/NTFS son dos ficheros y aquí también; en ext4 `+F` no, y eso es
        // el #145.
        let (zett, ss) = (
            corpus("ext4_full_fold_es_zett"),
            corpus("ext4_full_fold_ss"),
        );
        assert_ne!(key_for(&zett, mixto), key_for(&ss, mixto), "#145");
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
