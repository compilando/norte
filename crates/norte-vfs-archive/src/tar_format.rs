//! Indexado de tar plano (ustar/GNU/pax vía crate `tar`). SYNC: corre en
//! `spawn_blocking` sobre un [`ProviderReader`](crate::blocking::ProviderReader).

use std::io::{Read, Seek};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use norte_proto::{EntryKind, Error};

use crate::index::{ArchiveIndex, Limits, Locator, Node};

/// Milisegundos desde epoch a partir de los segundos (saturando: los mtime
/// basura de tars hostiles no panican). Deuda: los overrides pax de mtime
/// no se aplican (el crate `tar` solo sobreescribe size/uid/gid) — el mtime
/// mostrado es el del header ustar.
fn secs_to_ms(secs: u64) -> Option<i64> {
    i64::try_from(secs).ok()?.checked_mul(1000)
}

/// Construye el índice recorriendo los HEADERS del tar (`entries_with_seek`:
/// los datos se saltan con `Seek`, no se descargan — sobre un provider
/// remoto el coste es O(headers), no O(tamaño)).
///
/// `cancel` se chequea en el inner loop (regla 3): el lado async lo arma al
/// dropear el future y el hilo blocking termina en la siguiente entrada.
/// `container_len` valida cada locator contra el tamaño real: un tar
/// truncado no puede prometer datos más allá del contenedor.
pub(crate) fn build_index<R: Read + Seek>(
    reader: R,
    container_len: u64,
    generation: (Option<i64>, Option<u64>),
    limits: &Limits,
    cancel: &Arc<AtomicBool>,
) -> Result<ArchiveIndex, Error> {
    let mut index = ArchiveIndex::new(generation);
    let mut archive = tar::Archive::new(reader);
    let entries = archive.entries_with_seek().map_err(|e| corrupt(&e))?;
    for entry in entries {
        if cancel.load(Ordering::Relaxed) {
            tracing::debug!("indexado tar cancelado");
            return Err(Error::Cancelled);
        }
        let entry = entry.map_err(|e| corrupt(&e))?;
        // El iterador del crate `tar` consume L/K/x (longname/pax local)
        // pero NO `g`: sin esto, todo tar de `git archive` listaría un
        // `pax_global_header` fantasma (auditoría 8e, H5).
        if entry.header().entry_type() == tar::EntryType::XGlobalHeader {
            continue;
        }
        let raw_name = entry.path_bytes().to_vec();
        let header = entry.header();
        let kind = header.entry_type();
        let mtime_ms = header.mtime().ok().and_then(secs_to_ms);
        let node = match kind {
            tar::EntryType::Directory => Node::dir(mtime_ms),
            tar::EntryType::Symlink => Node {
                kind: EntryKind::Symlink,
                size: None,
                mtime_ms,
                locator: None,
                link_target: entry.link_name_bytes().map(|b| b.to_vec()),
            },
            tar::EntryType::Regular | tar::EntryType::Continuous => {
                let size = entry.size();
                let offset = entry.raw_file_position();
                if offset
                    .checked_add(size)
                    .is_none_or(|end| end > container_len)
                {
                    // Con seek los datos no se leen: un tar truncado se
                    // detecta validando el locator, no tropezando con EOF.
                    tracing::warn!("tar truncado: entrada promete datos más allá del contenedor");
                    return Err(Error::Corrupt);
                }
                Node {
                    kind: EntryKind::File,
                    size: Some(size),
                    mtime_ms,
                    locator: Some(Locator::Tar { offset, size }),
                    link_target: None,
                }
            }
            // Hardlinks, devices, fifos, GNU sparse…: se listan como Other
            // sin locator (read → Unsupported). Los tipos meta (long name,
            // pax headers) ya los consumió el iterador del crate `tar`.
            _ => Node {
                kind: EntryKind::Other,
                size: None,
                mtime_ms,
                locator: None,
                link_target: None,
            },
        };
        index.insert_entry(&raw_name, node, limits)?;
        // Las omitidas también consumen presupuesto: un tar de millones de
        // nombres hostiles no itera gratis (hallazgo M2 de fase 8d).
        if index.skipped > limits.max_entries as u64 {
            tracing::warn!(
                max = limits.max_entries,
                "tar supera el presupuesto de omitidas"
            );
            return Err(Error::Corrupt);
        }
    }
    if index.skipped > 0 {
        tracing::warn!(
            skipped = index.skipped,
            "entradas omitidas del índice (nombres hostiles/límites); detalle en debug"
        );
    }
    Ok(index)
}

fn corrupt(e: &std::io::Error) -> Error {
    // IO genuino del provider interior (corte de red a mitad de parseo):
    // se propaga VERBATIM, jamás se disfraza de Corrupt (#58).
    if let Some(inner) = crate::blocking::inner_proto_error(e) {
        return inner;
    }
    tracing::warn!(error = %e, "tar corrupto o ilegible");
    Error::Corrupt
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn limits() -> Limits {
        Limits::default()
    }

    /// La cancelación corta el loop en la siguiente entrada (regla 3).
    #[test]
    fn cancelacion_corta_el_indexado() {
        let mut smith = norte_testkit::TarSmith::new();
        for i in 0..50u32 {
            smith = smith.file(format!("f{i}").as_bytes(), b"x");
        }
        let bytes = smith.build();
        let len = bytes.len() as u64;
        let cancel = Arc::new(AtomicBool::new(true)); // armado ANTES
        let got = build_index(
            Cursor::new(bytes),
            len,
            (Some(0), Some(len)),
            &limits(),
            &cancel,
        );
        assert_eq!(got.map(|_| ()).unwrap_err(), Error::Cancelled);
    }

    /// Un locator que promete datos fuera del contenedor = truncado.
    #[test]
    fn locator_fuera_del_contenedor_es_corrupt() {
        let bytes = norte_testkit::TarSmith::new()
            .file(b"grande.bin", &[7u8; 2000])
            .build();
        let len = bytes.len() as u64;
        let cancel = Arc::new(AtomicBool::new(false));
        // Mentimos: el contenedor "mide" menos de lo que el header promete.
        let got = build_index(
            Cursor::new(bytes),
            700,
            (Some(0), Some(len)),
            &limits(),
            &cancel,
        );
        assert_eq!(got.map(|_| ()).unwrap_err(), Error::Corrupt);
    }
}
