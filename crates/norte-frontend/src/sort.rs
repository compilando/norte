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
