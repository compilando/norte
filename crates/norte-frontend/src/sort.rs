//! Orden del listado (presentación), compartido por los frontends.

use norte_proto::{Entry, EntryKind};

/// Orden elegido para el listado (#108 L7): columna + dirección + grupo de
/// dirs. El default reproduce EXACTAMENTE el orden histórico (name/asc/
/// dirs-first), así que nada cambia hasta que el usuario elige otra cosa.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct SortSpec {
    /// Columna por la que se ordena.
    pub column: SortColumn,
    /// Dirección: invierte SOLO la comparación de la columna — jamás el
    /// grupo de dirs ni el desempate por nombre (orden total y estable).
    pub dir: SortDir,
    /// Directorios primero (grupo aparte, siempre ascendente).
    pub dirs_first: bool,
}

impl Default for SortSpec {
    fn default() -> Self {
        Self {
            column: SortColumn::Name,
            dir: SortDir::Asc,
            dirs_first: true,
        }
    }
}

impl SortSpec {
    /// El resultado de un click en la cabecera de `col` (#108 b6): la columna
    /// activa invierte su dirección; una columna nueva ordena por ella
    /// ASCENDENTE. `dirs_first` jamás cambia por click — es una preferencia,
    /// no un criterio de columna. Compartido: la cabecera de la GUI hoy, los
    /// pickers de ambos frontends en el bloque 7.
    #[must_use]
    pub fn after_click(self, col: SortColumn) -> Self {
        if self.column == col {
            let dir = match self.dir {
                SortDir::Asc => SortDir::Desc,
                SortDir::Desc => SortDir::Asc,
            };
            Self { dir, ..self }
        } else {
            Self {
                column: col,
                dir: SortDir::Asc,
                ..self
            }
        }
    }
}

/// Columna de orden (#108). Solo built-ins por ahora — `attr:`/`plugin:`
/// llegan con los bloques 2/7 del diseño de columnas.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortColumn {
    /// Nombre (forma NFC como clave, bytes crudos de desempate) — el orden
    /// de siempre.
    Name,
    /// `Entry.size`. Un valor ausente (dirs, providers perezosos #52) va AL
    /// FINAL en ambas direcciones.
    Size,
    /// `Entry.mtime_ms` (negativos pre-1970 válidos). Ausente = al final.
    Mtime,
    /// Extensión del nombre: lo que sigue al ÚLTIMO punto (#138).
    ///
    /// Un directorio no tiene extensión, y un nombre que empieza por punto
    /// tampoco —`.bashrc` es un nombre, no una extensión—: las dos van AL
    /// FINAL en ambas direcciones, como cualquier valor ausente.
    Extension,
}

/// Dirección del orden de la columna.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortDir {
    /// Ascendente.
    Asc,
    /// Descendente (solo la columna; ver [`SortSpec::dir`]).
    Desc,
}

/// Orden del listado (presentación): directorios primero; dentro de cada
/// grupo, por la forma NFC del nombre (spec §6.1: `unicode_compare = nfc`
/// por defecto — SOLO como clave de orden, los bytes jamás se mutan) con
/// desempate por bytes crudos. Nombres no-UTF8: bytes tal cual.
pub fn sort_entries(entries: &mut [Entry]) {
    sort_entries_with(entries, SortSpec::default());
}

/// [`sort_entries`] bajo un [`SortSpec`] explícito (#108 L7). Estable;
/// mismo orden que `sort_with_keys_spec`/`merge_keyed_spec` con el mismo spec.
pub fn sort_entries_with(entries: &mut [Entry], spec: SortSpec) {
    let keys: Vec<SortKey> = entries.iter().map(sort_key).collect();
    // sort_by sobre índices sería más alloc-frugal, pero este camino solo
    // lo usan tests/CLI; los panes van por `sort_with_keys` (#54).
    let mut pares: Vec<(SortKey, Entry)> = keys.into_iter().zip(entries.iter().cloned()).collect();
    pares.sort_by(|a, b| cmp_keyed_with((&a.0, &a.1), (&b.0, &b.1), spec));
    for (slot, (_, e)) in entries.iter_mut().zip(pares) {
        *slot = e;
    }
}

/// Forma NFC del nombre como clave de orden, o `None` cuando coincide byte a
/// byte con el nombre crudo (ASCII, ya-NFC o no-UTF8) — el caso abrumadoramente
/// común, que así no materializa nada (#94).
fn nfc_key(name: &[u8]) -> Option<Vec<u8>> {
    use unicode_normalization::{UnicodeNormalization, is_nfc};
    let s = std::str::from_utf8(name).ok()?;
    if is_nfc(s) {
        return None;
    }
    Some(s.nfc().collect::<String>().into_bytes())
}

fn name_bytes(e: &Entry) -> &[u8] {
    e.path.file_name().map_or(b"", |n| n.as_bytes())
}

/// Clave de orden PERSISTIBLE de una entry (#54): grupo (dirs primero) +
/// forma NFC del nombre. El desempate por bytes crudos NO se materializa —
/// se lee del propio `Entry` al comparar (una alloc menos por entrada).
/// OJO: `PartialEq`/`Eq` derivados son REPRESENTACIONALES, no semánticos
/// (#94): un nombre ya-NFC (`nfc: None`) y su gemelo NFD (`nfc: Some(..)`)
/// tienen la MISMA clave efectiva pero `!=` como structs. Nada compara
/// `SortKey`s por igualdad para lógica — solo `cmp_keyed_with` define el orden.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SortKey {
    not_dir: bool,
    /// `None` = la clave NFC son los bytes crudos del nombre (caso común:
    /// ASCII, ya-NFC o no-UTF8) — se leen del propio `Entry` al comparar,
    /// sin materializar ~una alloc por entrada (#94).
    nfc: Option<Vec<u8>>,
}

pub(crate) fn sort_key(e: &Entry) -> SortKey {
    SortKey {
        not_dir: e.kind != EntryKind::Dir,
        nfc: nfc_key(name_bytes(e)),
    }
}

/// Comparación total (clave, entry): clave y, en empate, bytes crudos del
/// nombre — EXACTAMENTE el mismo orden que [`sort_entries`].
///
/// INVARIANTE: cada clave DEBE haberse computado de SU entry emparejada
/// ([`sort_key`]). Desde #94 el emparejamiento es load-bearing para la clave
/// PRIMARIA (un `None` se resuelve leyendo la entry) — una clave ajena ya no
/// corrompe solo el desempate, corrompe el orden en silencio.
/// `cmp_keyed_with` bajo un [`SortSpec`] (#108 L7). Orden, con cada regla
/// load-bearing:
/// 1. grupo dirs (si `dirs_first`) — SIEMPRE ascendente;
/// 2. la columna, invertida si `Desc`; un valor AUSENTE (dir sin size,
///    mtime desconocido) va al final EN AMBAS direcciones — el desc no
///    llena la cabecera del pane de blancos;
/// 3. desempate: el orden de nombre de siempre, SIEMPRE ascendente —
///    total, estable y determinista.
pub(crate) fn cmp_keyed_with(
    a: (&SortKey, &Entry),
    b: (&SortKey, &Entry),
    spec: SortSpec,
) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    debug_assert!(
        a.0.not_dir == (a.1.kind != EntryKind::Dir),
        "clave↔entry desparejadas"
    );
    debug_assert!(
        b.0.not_dir == (b.1.kind != EntryKind::Dir),
        "clave↔entry desparejadas"
    );
    if spec.dirs_first {
        let grupo = a.0.not_dir.cmp(&b.0.not_dir);
        if grupo != Ordering::Equal {
            return grupo;
        }
    }
    let col = match spec.column {
        SortColumn::Name => cmp_name(a, b),
        SortColumn::Size => cmp_missing_last(a.1.size, b.1.size, spec.dir),
        SortColumn::Mtime => cmp_missing_last(a.1.mtime_ms, b.1.mtime_ms, spec.dir),
        SortColumn::Extension => cmp_ext(a.1, b.1, spec.dir),
    };
    let col = match (spec.column, spec.dir) {
        // Name lleva el desempate integrado y su inversión es del bloque
        // entero (no hay «ausente» que anclar al final).
        (SortColumn::Name, SortDir::Desc) => col.reverse(),
        _ => col,
    };
    col.then_with(|| cmp_name(a, b))
}

/// La extensión de una entry: los bytes tras el ÚLTIMO punto, o `None`.
///
/// `None` para un directorio, para un nombre sin punto, para uno que EMPIEZA
/// por punto (`.bashrc` es un nombre entero) y para uno que acaba en punto (no
/// hay nada detrás). Devuelve BYTES, no texto: un nombre no tiene por qué ser
/// UTF-8 (regla 1).
fn ext_bytes(e: &Entry) -> Option<&[u8]> {
    if e.kind == EntryKind::Dir {
        return None;
    }
    let name = name_bytes(e);
    let punto = name.iter().rposition(|b| *b == b'.')?;
    if punto == 0 || punto + 1 == name.len() {
        return None;
    }
    Some(&name[punto + 1..])
}

/// Compara dos extensiones SIN distinguir mayúsculas.
///
/// `.TXT` y `.txt` son la misma extensión para quien ordena —agruparlas es el
/// punto entero de ordenar por extensión— aunque sigan siendo nombres
/// distintos para todo lo demás: la identidad jamás se pliega (ADR 0051), el
/// ORDEN sí.
///
/// El camino común (extensión ASCII) no reserva memoria; el raro delega en el
/// mismo `fold` que usa la búsqueda rápida, para no inventar un segundo
/// vocabulario de plegado.
fn cmp_ext_bytes(a: &[u8], b: &[u8]) -> std::cmp::Ordering {
    if a.is_ascii() && b.is_ascii() {
        return a
            .iter()
            .map(u8::to_ascii_lowercase)
            .cmp(b.iter().map(u8::to_ascii_lowercase));
    }
    crate::nav::fold(a).cmp(&crate::nav::fold(b))
}

/// La columna EXTENSIÓN: ausente al final en las dos direcciones, igual que un
/// tamaño o una fecha que no se conocen.
fn cmp_ext(a: &Entry, b: &Entry, dir: SortDir) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (ext_bytes(a), ext_bytes(b)) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(x), Some(y)) => {
            let o = cmp_ext_bytes(x, y);
            match dir {
                SortDir::Asc => o,
                SortDir::Desc => o.reverse(),
            }
        }
    }
}

/// El orden de NOMBRE de siempre: clave NFC y desempate por bytes crudos.
fn cmp_name(a: (&SortKey, &Entry), b: (&SortKey, &Entry)) -> std::cmp::Ordering {
    let nfc_a = a.0.nfc.as_deref().unwrap_or_else(|| name_bytes(a.1));
    let nfc_b = b.0.nfc.as_deref().unwrap_or_else(|| name_bytes(b.1));
    nfc_a
        .cmp(nfc_b)
        .then_with(|| name_bytes(a.1).cmp(name_bytes(b.1)))
}

/// Comparación de una columna opcional con «ausente al final en AMBAS
/// direcciones»: los presentes se comparan (invertidos si `Desc`), un
/// ausente pierde contra cualquier presente, dos ausentes empatan (decide
/// el desempate por nombre del caller).
fn cmp_missing_last<T: Ord>(lhs: Option<T>, rhs: Option<T>, dir: SortDir) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (lhs, rhs) {
        (Some(lv), Some(rv)) => {
            let ord = lv.cmp(&rv);
            if dir == SortDir::Desc {
                ord.reverse()
            } else {
                ord
            }
        }
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

/// Ordena `entries` computando sus claves UNA vez y devuelve ambas
/// (índice-paralelas). Estable, mismo orden que [`sort_entries`].
/// `sort_with_keys_spec` bajo un [`SortSpec`] (#108 L7).
pub(crate) fn sort_with_keys_spec(
    entries: Vec<Entry>,
    spec: SortSpec,
) -> (Vec<Entry>, Vec<SortKey>) {
    let mut pares: Vec<(SortKey, Entry)> = entries.into_iter().map(|e| (sort_key(&e), e)).collect();
    pares.sort_by(|a, b| cmp_keyed_with((&a.0, &a.1), (&b.0, &b.1), spec));
    pares.into_iter().map(|(k, e)| (e, k)).unzip()
}

/// `merge_keyed_spec` bajo un [`SortSpec`] (#108 L7): AMBOS runs deben venir
/// ordenados por el MISMO spec.
pub(crate) fn merge_keyed_spec(
    entries: &mut Vec<Entry>,
    keys: &mut Vec<SortKey>,
    batch_entries: Vec<Entry>,
    batch_keys: Vec<SortKey>,
    spec: SortSpec,
) {
    // El zip de abajo TRUNCARÍA en silencio si los paralelos se desincronizan
    // (pérdida de entradas del listado sin ruido): que un bug futuro falle
    // ruidoso en dev/test, no calladamente en producción.
    debug_assert_eq!(
        entries.len(),
        keys.len(),
        "entries↔sort_keys desincronizados"
    );
    debug_assert_eq!(
        batch_entries.len(),
        batch_keys.len(),
        "lote↔claves desincronizados"
    );
    let mut out_e = Vec::with_capacity(entries.len() + batch_entries.len());
    let mut out_k = Vec::with_capacity(keys.len() + batch_keys.len());
    let mut left = std::mem::take(entries)
        .into_iter()
        .zip(std::mem::take(keys))
        .peekable();
    let mut right = batch_entries.into_iter().zip(batch_keys).peekable();
    loop {
        match (left.peek(), right.peek()) {
            (Some(l), Some(r)) => {
                // Izquierda gana el empate (estabilidad).
                if cmp_keyed_with((&l.1, &l.0), (&r.1, &r.0), spec) == std::cmp::Ordering::Greater {
                    let (e, k) = right.next().expect("peek == Some");
                    out_e.push(e);
                    out_k.push(k);
                } else {
                    let (e, k) = left.next().expect("peek == Some");
                    out_e.push(e);
                    out_k.push(k);
                }
            }
            (Some(_), None) => {
                for (e, k) in left.by_ref() {
                    out_e.push(e);
                    out_k.push(k);
                }
            }
            (None, Some(_)) => {
                for (e, k) in right.by_ref() {
                    out_e.push(e);
                    out_k.push(k);
                }
            }
            (None, None) => break,
        }
    }
    *entries = out_e;
    *keys = out_k;
}

#[cfg(test)]
mod tests {
    use super::*;
    use norte_proto::VPath;

    fn e(w: &str, k: EntryKind) -> Entry {
        Entry {
            attrs: std::collections::BTreeMap::new(),
            path: VPath::parse(w).unwrap(),
            kind: k,
            size: None,
            mtime_ms: None,
        }
    }

    /// Pin del contrato de `SortKey.nfc` (#94): `None` en el caso común
    /// (ASCII, ya-NFC, no-UTF8) — cero allocs persistidas — y `Some` SOLO
    /// cuando la forma NFC difiere de los bytes crudos (p.ej. NFD).
    #[test]
    fn sort_key_no_materializa_nfc_en_el_caso_comun() {
        // El caso q+U+0300 es quick-check=Maybe pero SÍ es NFC (no hay
        // precompuesta): mata al mutante `is_nfc_quick(..) == Yes`, que
        // materializaría toda marca combinante y regresaría el cero-alloc
        // en silencio (encoding-auditor m1).
        for w in [
            "mem:///ascii.txt",
            "mem:///a%C3%B1o",
            "mem:///%FF%FE",
            "mem:///q%CC%80",
        ] {
            let k = sort_key(&e(w, EntryKind::File));
            assert_eq!(k.nfc, None, "{w}: nfc==bytes crudos, no debe alocar");
        }
        let nfd = sort_key(&e("mem:///an%CC%83o", EntryKind::File));
        assert_eq!(
            nfd.nfc.as_deref(),
            Some("año".as_bytes()),
            "NFD materializa su forma NFC"
        );
        // Singleton U+212B (ANGSTROM SIGN) → U+00C5 "Å": NFC difiere sin
        // ser el caso NFD clásico — debe materializar.
        let singleton = sort_key(&e("mem:///%E2%84%AB", EntryKind::File));
        assert_eq!(
            singleton.nfc.as_deref(),
            Some("Å".as_bytes()),
            "singleton materializa su forma NFC"
        );
    }

    /// Equivalencia determinista (sin proptest como dev-dep en este crate,
    /// ver `grep proptest crates/norte-frontend/Cargo.toml`): dos mitades
    /// desordenadas, cada una pasada por `sort_with_keys`, mergeadas con
    /// `merge_keyed`, deben coincidir EXACTAMENTE con `sort_entries` sobre el
    /// total — cubre dirs/files, NFD vs NFC, no-UTF8 y empates de clave.
    #[test]
    fn merge_keyed_equivale_a_sort_entries() {
        let izquierda = vec![
            e("mem:///zeta", EntryKind::File),
            e("mem:///Adir", EntryKind::Dir),
            e("mem:///an%CC%83o", EntryKind::File), // NFD "año"
            e("mem:///%FF%FE", EntryKind::File),    // no-UTF8
        ];
        let derecha = vec![
            e("mem:///a%C3%B1o2", EntryKind::File), // NFC "año2"
            e("mem:///Bdir", EntryKind::Dir),
            e("mem:///a%C3%B1o", EntryKind::File), // NFC "año" — empata clave con la NFD de arriba
            e("mem:///alfa", EntryKind::File),
        ];

        let (mut entries, mut keys) = sort_with_keys_spec(izquierda.clone(), SortSpec::default());
        let (batch_entries, batch_keys) = sort_with_keys_spec(derecha.clone(), SortSpec::default());
        merge_keyed_spec(
            &mut entries,
            &mut keys,
            batch_entries,
            batch_keys,
            SortSpec::default(),
        );

        let mut esperado: Vec<Entry> = izquierda.into_iter().chain(derecha).collect();
        sort_entries(&mut esperado);

        assert_eq!(entries, esperado, "merge_keyed ≡ sort_entries del total");
        assert_eq!(keys.len(), entries.len());
    }

    #[test]
    fn after_click_misma_columna_invierte_la_direccion() {
        let s = SortSpec {
            column: SortColumn::Size,
            dir: SortDir::Asc,
            dirs_first: true,
        };
        let t = s.after_click(SortColumn::Size);
        assert_eq!(
            t,
            SortSpec {
                column: SortColumn::Size,
                dir: SortDir::Desc,
                dirs_first: true,
            }
        );
        // Y el segundo click vuelve a Asc.
        assert_eq!(t.after_click(SortColumn::Size).dir, SortDir::Asc);
    }

    #[test]
    fn after_click_columna_nueva_asc_y_dirs_first_intacto() {
        let s = SortSpec {
            column: SortColumn::Name,
            dir: SortDir::Desc,
            dirs_first: false,
        };
        let t = s.after_click(SortColumn::Mtime);
        assert_eq!(
            t,
            SortSpec {
                column: SortColumn::Mtime,
                dir: SortDir::Asc,
                dirs_first: false,
            }
        );
    }
}

#[cfg(test)]
mod sort_spec_merge {
    use super::*;
    use norte_proto::{EntryKind, VPath};

    fn e(name: &str, dir: bool, size: Option<u64>, mtime: Option<i64>) -> Entry {
        Entry {
            attrs: std::collections::BTreeMap::new(),
            path: VPath::parse(&format!("mem:///{name}")).unwrap(),
            kind: if dir { EntryKind::Dir } else { EntryKind::File },
            size,
            mtime_ms: mtime,
        }
    }

    fn all_specs() -> Vec<SortSpec> {
        let mut out = Vec::new();
        for column in [SortColumn::Name, SortColumn::Size, SortColumn::Mtime] {
            for dir in [SortDir::Asc, SortDir::Desc] {
                for dirs_first in [false, true] {
                    out.push(SortSpec {
                        column,
                        dir,
                        dirs_first,
                    });
                }
            }
        }
        out
    }

    /// #108 L7: merge incremental ≡ sort total bajo LOS 12 specs — la
    /// propiedad #54, generalizada del default al espacio entero, sobre un
    /// corpus con ausentes, empates, dirs y pre-1970, partido en todos los
    /// puntos posibles.
    #[test]
    fn merge_equivale_a_sort_bajo_todos_los_specs() {
        let corpus = vec![
            e("b", false, Some(10), Some(5)),
            e("dir1", true, None, Some(-3)),
            e("a", false, Some(10), None),
            e("z", false, None, Some(5)),
            e("dir2", true, Some(4096), None),
            e("m", false, Some(1), Some(1_000)),
            e("a2", false, None, None),
        ];
        for spec in all_specs() {
            for cut in 0..=corpus.len() {
                let (izq, der) = corpus.split_at(cut);
                let mut total = corpus.clone();
                sort_entries_with(&mut total, spec);

                let (mut entries, mut keys) = sort_with_keys_spec(izq.to_vec(), spec);
                let (be, bk) = sort_with_keys_spec(der.to_vec(), spec);
                merge_keyed_spec(&mut entries, &mut keys, be, bk, spec);
                let a: Vec<_> = total.iter().map(|x| x.path.clone()).collect();
                let b: Vec<_> = entries.iter().map(|x| x.path.clone()).collect();
                assert_eq!(a, b, "spec {spec:?} corte {cut}");
            }
        }
    }
}

#[cfg(test)]
mod sort_spec_tests {
    use super::*;
    use norte_proto::{EntryKind, VPath};

    fn e(name: &str, kind: EntryKind, size: Option<u64>, mtime: Option<i64>) -> Entry {
        Entry {
            attrs: std::collections::BTreeMap::new(),
            path: VPath::parse(&format!("mem:///{name}")).unwrap(),
            kind,
            size,
            mtime_ms: mtime,
        }
    }

    fn names(entries: &[Entry]) -> Vec<&[u8]> {
        entries
            .iter()
            .map(|e| e.path.file_name().map_or(&b""[..], |n| n.as_bytes()))
            .collect()
    }

    /// #108 L7: por tamaño ASC — dirs primero (siempre), None AL FINAL,
    /// desempate por nombre.
    #[test]
    fn size_asc_none_al_final_y_dirs_primero() {
        let mut es = vec![
            e("g", EntryKind::File, Some(5), None),
            e("f-sin", EntryKind::File, None, None),
            e("a", EntryKind::File, Some(9), None),
            e("dir", EntryKind::Dir, None, None),
            e("b", EntryKind::File, Some(5), None),
        ];
        let spec = SortSpec {
            column: SortColumn::Size,
            dir: SortDir::Asc,
            dirs_first: true,
        };
        sort_entries_with(&mut es, spec);
        assert_eq!(
            names(&es),
            vec![
                b"dir".as_slice(), // grupo dirs
                b"b",              // 5, desempate nombre asc
                b"g",              // 5
                b"a",              // 9
                b"f-sin",          // None SIEMPRE al final
            ]
        );
    }

    /// #108 L7: DESC invierte SOLO la columna — None sigue al final, el
    /// desempate sigue por nombre ASC, el grupo dirs no se invierte.
    #[test]
    fn size_desc_invierte_solo_la_columna() {
        let mut es = vec![
            e("g", EntryKind::File, Some(5), None),
            e("f-sin", EntryKind::File, None, None),
            e("a", EntryKind::File, Some(9), None),
            e("dir", EntryKind::Dir, None, None),
            e("b", EntryKind::File, Some(5), None),
        ];
        let spec = SortSpec {
            column: SortColumn::Size,
            dir: SortDir::Desc,
            dirs_first: true,
        };
        sort_entries_with(&mut es, spec);
        assert_eq!(
            names(&es),
            vec![
                b"dir".as_slice(),
                b"a",     // 9
                b"b",     // 5, desempate nombre ASC aunque la columna sea desc
                b"g",     // 5
                b"f-sin", // None al final TAMBIÉN en desc
            ]
        );
    }

    /// #108 L7: mtime desc = «lo recién descargado arriba», negativos
    /// (pre-1970) válidos.
    #[test]
    fn mtime_desc_con_pre_1970() {
        let mut es = vec![
            e("viejo", EntryKind::File, None, Some(-1000)),
            e("nuevo", EntryKind::File, None, Some(2_000_000)),
            e("sin", EntryKind::File, None, None),
            e("medio", EntryKind::File, None, Some(1_000)),
        ];
        let spec = SortSpec {
            column: SortColumn::Mtime,
            dir: SortDir::Desc,
            dirs_first: true,
        };
        sort_entries_with(&mut es, spec);
        assert_eq!(
            names(&es),
            vec![b"nuevo".as_slice(), b"medio", b"viejo", b"sin"]
        );
    }

    /// El spec DEFAULT reproduce el orden histórico exacto.
    #[test]
    fn spec_default_es_el_orden_de_siempre() {
        let mut a = vec![
            e("b", EntryKind::File, Some(1), None),
            e("dir", EntryKind::Dir, None, None),
            e("a", EntryKind::File, Some(2), None),
        ];
        let mut b = a.clone();
        sort_entries(&mut a);
        sort_entries_with(&mut b, SortSpec::default());
        assert_eq!(a, b);
    }

    /// `dirs_first` = false: los dirs compiten como uno más.
    #[test]
    fn sin_dirs_first_no_hay_grupo() {
        let mut es = vec![
            e("z-dir", EntryKind::Dir, None, None),
            e("a", EntryKind::File, None, None),
        ];
        let spec = SortSpec {
            column: SortColumn::Name,
            dir: SortDir::Asc,
            dirs_first: false,
        };
        sort_entries_with(&mut es, spec);
        assert_eq!(names(&es), vec![b"a".as_slice(), b"z-dir"]);
    }
}

#[cfg(test)]
mod extension_tests {
    use super::*;

    fn e(nombre: &str, kind: EntryKind) -> Entry {
        crudo(nombre.as_bytes(), kind)
    }

    fn crudo(nombre: &[u8], kind: EntryKind) -> Entry {
        let seg = norte_proto::Segment::new(nombre.to_vec()).expect("segmento");
        Entry {
            path: norte_proto::VPath::parse("file:///d")
                .expect("vpath")
                .join(seg),
            kind,
            size: None,
            mtime_ms: None,
            attrs: std::collections::BTreeMap::new(),
        }
    }

    fn nombres(v: &[Entry]) -> Vec<String> {
        v.iter()
            .map(|e| String::from_utf8_lossy(name_bytes(e)).into_owned())
            .collect()
    }

    fn por_extension(dir: SortDir) -> SortSpec {
        SortSpec {
            column: SortColumn::Extension,
            dir,
            dirs_first: true,
        }
    }

    /// `.TXT` va con `.txt`: agrupar las dos es el punto entero de ordenar por
    /// extensión. La identidad no se pliega jamás (ADR 0051); el ORDEN sí.
    #[test]
    fn la_extension_agrupa_sin_distinguir_mayusculas() {
        let mut v = vec![
            e("b.TXT", EntryKind::File),
            e("a.rs", EntryKind::File),
            e("c.txt", EntryKind::File),
        ];
        sort_entries_with(&mut v, por_extension(SortDir::Asc));
        assert_eq!(nombres(&v), ["a.rs", "b.TXT", "c.txt"]);
    }

    /// Lo que no tiene extensión va AL FINAL en las DOS direcciones, como
    /// cualquier ausente: un `desc` no llena la cabecera del pane de nombres
    /// pelados. Y un nombre que EMPIEZA por punto no tiene extensión —
    /// `.bashrc` es un nombre entero—, ni uno que acaba en punto.
    #[test]
    fn lo_que_no_tiene_extension_va_al_final_en_las_dos_direcciones() {
        for dir in [SortDir::Asc, SortDir::Desc] {
            let mut v = vec![
                e("leeme", EntryKind::File),
                e(".bashrc", EntryKind::File),
                e("a.rs", EntryKind::File),
                e("punto.", EntryKind::File),
            ];
            sort_entries_with(&mut v, por_extension(dir));
            assert_eq!(nombres(&v)[0], "a.rs", "el único con extensión manda ({dir:?})");
        }
    }

    /// Una extensión que no es UTF-8 ordena igual y no muta un byte (regla 1).
    #[test]
    fn una_extension_no_utf8_ordena_sin_romperse() {
        let mut v = vec![
            crudo(b"raro\xff.zz", EntryKind::File),
            e("a.aa", EntryKind::File),
        ];
        sort_entries_with(&mut v, por_extension(SortDir::Asc));
        assert_eq!(nombres(&v)[0], "a.aa", "aa < zz");
        assert_eq!(
            name_bytes(&v[1]),
            b"raro\xff.zz",
            "y los bytes vuelven intactos"
        );
    }

    /// El grupo de directorios manda antes que la columna, como con cualquier
    /// otra: un dir no tiene extensión y aun así sale primero.
    #[test]
    fn los_directorios_siguen_primero() {
        let mut v = vec![e("z.rs", EntryKind::File), e("dir", EntryKind::Dir)];
        sort_entries_with(&mut v, por_extension(SortDir::Asc));
        assert_eq!(v[0].kind, EntryKind::Dir);
    }

    /// Empate de extensión: desempata el nombre, SIEMPRE ascendente, como en
    /// las demás columnas.
    #[test]
    fn a_igual_extension_desempata_el_nombre() {
        let mut v = vec![
            e("z.rs", EntryKind::File),
            e("a.rs", EntryKind::File),
            e("m.rs", EntryKind::File),
        ];
        sort_entries_with(&mut v, por_extension(SortDir::Desc));
        assert_eq!(nombres(&v), ["a.rs", "m.rs", "z.rs"]);
    }
}
