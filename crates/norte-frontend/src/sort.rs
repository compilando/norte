//! Orden del listado (presentación), compartido por los frontends.

use norte_proto::{Entry, EntryKind};

/// Orden del listado (presentación): directorios primero; dentro de cada
/// grupo, por la forma NFC del nombre (spec §6.1: `unicode_compare = nfc`
/// por defecto — SOLO como clave de orden, los bytes jamás se mutan) con
/// desempate por bytes crudos. Nombres no-UTF8: bytes tal cual.
pub fn sort_entries(entries: &mut [Entry]) {
    entries.sort_by_cached_key(|e| {
        let name = name_bytes(e);
        (e.kind != EntryKind::Dir, nfc_key(name), name.to_vec())
    });
}

fn nfc_key(name: &[u8]) -> Vec<u8> {
    use unicode_normalization::UnicodeNormalization;
    match std::str::from_utf8(name) {
        Ok(s) => s.nfc().collect::<String>().into_bytes(),
        Err(_) => name.to_vec(),
    }
}

fn name_bytes(e: &Entry) -> &[u8] {
    e.path.file_name().map_or(b"", |n| n.as_bytes())
}

/// Clave de orden PERSISTIBLE de una entry (#54): grupo (dirs primero) +
/// forma NFC del nombre. El desempate por bytes crudos NO se materializa —
/// se lee del propio `Entry` al comparar (una alloc menos por entrada).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SortKey {
    not_dir: bool,
    nfc: Vec<u8>,
}

pub(crate) fn sort_key(e: &Entry) -> SortKey {
    SortKey {
        not_dir: e.kind != EntryKind::Dir,
        nfc: nfc_key(name_bytes(e)),
    }
}

/// Comparación total (clave, entry): clave y, en empate, bytes crudos del
/// nombre — EXACTAMENTE el mismo orden que [`sort_entries`].
pub(crate) fn cmp_keyed(a: (&SortKey, &Entry), b: (&SortKey, &Entry)) -> std::cmp::Ordering {
    (a.0.not_dir, &a.0.nfc)
        .cmp(&(b.0.not_dir, &b.0.nfc))
        .then_with(|| name_bytes(a.1).cmp(name_bytes(b.1)))
}

/// Ordena `entries` computando sus claves UNA vez y devuelve ambas
/// (índice-paralelas). Estable, mismo orden que [`sort_entries`].
pub(crate) fn sort_with_keys(entries: Vec<Entry>) -> (Vec<Entry>, Vec<SortKey>) {
    let mut pares: Vec<(SortKey, Entry)> = entries.into_iter().map(|e| (sort_key(&e), e)).collect();
    pares.sort_by(|a, b| cmp_keyed((&a.0, &a.1), (&b.0, &b.1)));
    pares.into_iter().map(|(k, e)| (e, k)).unzip()
}

/// Merge ESTABLE de dos runs ordenados (izquierda gana el empate — el run
/// existente conserva su posición relativa, como el sort estable de antes).
/// O(n+m) sin recomputar claves.
pub(crate) fn merge_keyed(
    entries: &mut Vec<Entry>,
    keys: &mut Vec<SortKey>,
    batch_entries: Vec<Entry>,
    batch_keys: Vec<SortKey>,
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
                if cmp_keyed((&l.1, &l.0), (&r.1, &r.0)) == std::cmp::Ordering::Greater {
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
            path: VPath::parse(w).unwrap(),
            kind: k,
            size: None,
            mtime_ms: None,
        }
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

        let (mut entries, mut keys) = sort_with_keys(izquierda.clone());
        let (batch_entries, batch_keys) = sort_with_keys(derecha.clone());
        merge_keyed(&mut entries, &mut keys, batch_entries, batch_keys);

        let mut esperado: Vec<Entry> = izquierda.into_iter().chain(derecha).collect();
        sort_entries(&mut esperado);

        assert_eq!(entries, esperado, "merge_keyed ≡ sort_entries del total");
        assert_eq!(keys.len(), entries.len());
    }
}
