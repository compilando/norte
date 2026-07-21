//! Indexado y lectura de tar.gz/tgz (ADR 0028, #55): `tar+gz` compuesto —
//! la capa gz es OPACA sobre tar, forward-only (no seekable). El índice
//! recorre `tar::Archive::entries()` (solo `Read`, sin `entries_with_seek`)
//! sobre un `flate2::read::MultiGzDecoder` (miembros gzip concatenados: los
//! tgz reales los tienen); los offsets de `Locator::Gz` son del stream
//! DESCOMPRIMIDO. SYNC: corre en `spawn_blocking` sobre un
//! [`ProviderReader`](crate::blocking::ProviderReader), igual que
//! [`tar_format`](crate::tar_format)/[`zip_format`](crate::zip_format) —
//! de hecho reutiliza la clasificación de entradas de `tar_format`
//! (`classify_entry`/`EntryShape`): nombre/kind/symlink/mtime son
//! IDÉNTICOS al tar plano, solo cambia cómo se resuelve el `Locator` de un
//! archivo regular.

use std::io::Read;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use flate2::read::MultiGzDecoder;
use norte_proto::{EntryKind, Error};

use crate::index::{ArchiveIndex, Limits, Locator, Node};
use crate::tar_format::{EntryShape, classify_entry};

/// Envuelve el DECODER gzip (no el `Read` crudo del contenedor) para contar
/// bytes DESCOMPRIMIDOS leídos: una gzip bomb es CPU infinita aunque la
/// memoria del pipeline sea streaming (el decoder nunca materializa el
/// contenido completo) — `max` corta el PASE DE ÍNDICE entero (ADR 0028
/// D4), no por entrada.
///
/// También arma la cancelación (regla 3) POR CHUNK, no solo por entrada: el
/// `tar::Entries` interno puede consumir el cuerpo completo de una entrada
/// (o el descarte de sus bytes sobrantes al saltar a la siguiente) SIN
/// devolver el control al loop externo de `build_index_gz` — chequear
/// `cancel` solo entre entradas no bastaría para cortar rápido una entrada
/// gigante.
struct CountingReader<R> {
    inner: R,
    read_total: u64,
    max: u64,
    cancel: Arc<AtomicBool>,
}

impl<R: Read> Read for CountingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err(std::io::Error::other(Error::Cancelled));
        }
        let n = self.inner.read(buf)?;
        self.read_total += n as u64;
        if self.read_total > self.max {
            tracing::warn!(
                max = self.max,
                "tar.gz supera el presupuesto de descompresión del índice (bomba)"
            );
            return Err(std::io::Error::other(Error::Corrupt));
        }
        Ok(n)
    }
}

/// Construye el índice de un tar.gz recorriendo `entries()` (forward-only,
/// sin `Seek`) sobre el `MultiGzDecoder`. Reutiliza `classify_entry` de
/// [`tar_format`](crate::tar_format) para nombre/kind/symlink/mtime; solo
/// difiere en el `Locator` del archivo regular: offset DESCOMPRIMIDO, SIN
/// validar contra ningún `container_len` (ADR 0028 — ese tamaño sería el
/// COMPRIMIDO y no acota nada del stream descomprimido). El truncamiento se
/// detecta fail-loud en el propio `entries()` cuando el decoder corta a
/// mitad de una entrada (vía `corrupt`, mismo criterio #58 que tar/zip:
/// el IO genuino del provider interior se propaga verbatim).
///
/// `cancel` se chequea por entrada (paridad con el tar plano) Y por chunk
/// dentro de `CountingReader` (más fino: una sola entrada gigante no debe
/// bloquear la cancelación).
pub(crate) fn build_index_gz<R: Read>(
    reader: R,
    generation: (Option<i64>, Option<u64>),
    limits: &Limits,
    cancel: &Arc<AtomicBool>,
) -> Result<ArchiveIndex, Error> {
    let counting = CountingReader {
        inner: MultiGzDecoder::new(reader),
        read_total: 0,
        max: limits.max_decompressed_bytes,
        cancel: Arc::clone(cancel),
    };
    let mut index = ArchiveIndex::new(generation);
    let mut archive = tar::Archive::new(counting);
    let entries = archive.entries().map_err(|e| corrupt(&e))?;
    for entry in entries {
        if cancel.load(Ordering::Relaxed) {
            tracing::debug!("indexado tar.gz cancelado");
            return Err(Error::Cancelled);
        }
        let entry = entry.map_err(|e| corrupt(&e))?;
        let Some((raw_name, mtime_ms, shape)) = classify_entry(&entry) else {
            continue; // meta ya consumida por el iterador (pax_global_header)
        };
        let node = match shape {
            EntryShape::Dir => Node::dir(mtime_ms),
            EntryShape::Symlink(link_target) => Node {
                kind: EntryKind::Symlink,
                size: None,
                mtime_ms,
                locator: None,
                link_target,
            },
            EntryShape::File { offset, size } => Node {
                kind: EntryKind::File,
                size: Some(size),
                mtime_ms,
                locator: Some(Locator::Gz { offset, size }),
                link_target: None,
            },
            EntryShape::Other => Node {
                kind: EntryKind::Other,
                size: None,
                mtime_ms,
                locator: None,
                link_target: None,
            },
        };
        index.insert_entry(&raw_name, node, limits)?;
        // Las omitidas también consumen presupuesto: un tar.gz de millones
        // de nombres hostiles no itera gratis (mismo criterio que tar/zip).
        if index.skipped > limits.max_entries as u64 {
            tracing::warn!(
                max = limits.max_entries,
                "tar.gz supera el presupuesto de omitidas"
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

/// Lee `take` bytes descomprimidos desde `skip` de un tar.gz. Decoder
/// FRESCO por lectura (gz no es seekable, ADR 0028 D3): descarta `skip`
/// bytes en chunks — CHEQUEANDO `tx.is_closed()` en cada chunk, porque un
/// descarte largo (offset grande dentro de una entrada) también debe ser
/// cancelable (regla 3; a diferencia del descarte de zip, que es corto por
/// la ventana de deflate) — y luego sirve `take` en chunks de 64 KiB por el
/// canal acotado. EOF antes de completar el descarte es pread fuera de
/// rango (stream vacío, sin error, mismo criterio que zip); EOF a mitad de
/// `take` es contenedor truncado bajo datos que el índice prometió: error
/// por el canal, jamás datos cortos en silencio.
pub(crate) fn read_entry_gz<R: Read>(
    reader: R,
    skip: u64,
    take: u64,
    tx: &tokio::sync::mpsc::Sender<Result<bytes::Bytes, Error>>,
) {
    let send_err = |tx: &tokio::sync::mpsc::Sender<Result<bytes::Bytes, Error>>, e: Error| {
        // Mejor esfuerzo: si el receptor murió, no hay a quién contárselo.
        let _ = tx.blocking_send(Err(e));
    };
    let mut decoder = MultiGzDecoder::new(reader);
    let mut buf = vec![0u8; 64 * 1024];
    let mut to_skip = skip;
    while to_skip > 0 {
        if tx.is_closed() {
            tracing::debug!("descarte de tar.gz cancelado (receptor muerto)");
            return;
        }
        let want = buf.len().min(usize::try_from(to_skip).unwrap_or(buf.len()));
        match decoder.read(&mut buf[..want]) {
            Ok(0) => return, // EOF antes del offset: stream vacío (pread)
            Ok(n) => to_skip -= n as u64,
            Err(e) => return send_err(tx, corrupt(&e)),
        }
    }
    let mut remaining = take;
    while remaining > 0 {
        let want = buf
            .len()
            .min(usize::try_from(remaining).unwrap_or(buf.len()));
        match decoder.read(&mut buf[..want]) {
            Ok(0) => {
                // Premature EOF a mitad de la entrada: el índice prometió
                // `size` bytes y el decoder no los tiene — contenedor
                // truncado bajo la entrada (jamás datos cortos en silencio).
                return send_err(tx, Error::Corrupt);
            }
            Ok(n) => {
                remaining -= n as u64;
                if tx
                    .blocking_send(Ok(bytes::Bytes::copy_from_slice(&buf[..n])))
                    .is_err()
                {
                    tracing::debug!("lectura tar.gz cancelada (receptor muerto)");
                    return;
                }
            }
            Err(e) => return send_err(tx, corrupt(&e)),
        }
    }
}

fn corrupt(e: &std::io::Error) -> Error {
    // IO genuino del provider interior (corte de red a mitad de parseo) O
    // señal envuelta por `CountingReader` (`Cancelled`/`Corrupt` de bomba):
    // ambos van por el mismo `io::Error::other`, se desenvuelven verbatim
    // (#58) — jamás se disfrazan de "tar.gz corrupto".
    if let Some(inner) = crate::blocking::inner_proto_error(e) {
        return inner;
    }
    tracing::warn!(error = %e, "tar.gz corrupto o ilegible");
    Error::Corrupt
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write as _};

    fn limits() -> Limits {
        Limits::default()
    }

    /// Gzipea bytes ya armados (p. ej. un tar de `TarSmith`) en un único
    /// miembro gzip.
    fn gzip(bytes: &[u8]) -> Vec<u8> {
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(bytes).expect("write gz");
        enc.finish().expect("finish gz")
    }

    /// Verificación exigida por el plan (#55 T3): `entry.raw_file_position()`
    /// SIRVE como offset DESCOMPRIMIDO con `entries()` (sin `Seek`) — el
    /// crate `tar` lo calcula contando bytes consumidos del `Read`, no vía
    /// `Seek::stream_position`. Dos archivos con contenido conocido: el
    /// segundo NO puede estar en offset 0, y el offset debe coincidir con
    /// el que produce `entries_with_seek` sobre el MISMO tar plano.
    #[test]
    fn raw_file_position_es_correcto_sin_seek() {
        let tar = norte_testkit::TarSmith::new()
            .file(b"primero.bin", &[0xAAu8; 600])
            .file(b"segundo.bin", b"0123456789")
            .build();

        // Offsets de referencia: con Seek sobre el tar PLANO (sin gz).
        let mut archive_seek = tar::Archive::new(Cursor::new(tar.clone()));
        let seek_offsets: Vec<(Vec<u8>, u64, u64)> = archive_seek
            .entries_with_seek()
            .expect("entries_with_seek")
            .map(|e| {
                let e = e.expect("entry");
                (e.path_bytes().to_vec(), e.raw_file_position(), e.size())
            })
            .collect();
        assert_eq!(
            seek_offsets.len(),
            2,
            "dos archivos en el tar de referencia"
        );
        assert!(
            seek_offsets[1].1 > 0,
            "el segundo archivo no puede empezar en offset 0"
        );

        // Mismos offsets, ahora vía `entries()` (sin Seek) sobre el tar PLANO
        // directo (sin gz de por medio: aísla la propiedad de raw_file_position).
        let mut archive_plain = tar::Archive::new(Cursor::new(tar.clone()));
        let plain_offsets: Vec<(Vec<u8>, u64, u64)> = archive_plain
            .entries()
            .expect("entries")
            .map(|e| {
                let e = e.expect("entry");
                (e.path_bytes().to_vec(), e.raw_file_position(), e.size())
            })
            .collect();
        assert_eq!(
            plain_offsets, seek_offsets,
            "raw_file_position() coincide entre entries() y entries_with_seek()"
        );

        // Y ahora vía el pipeline real: gz + MultiGzDecoder + entries().
        let gz = gzip(&tar);
        let mut archive_gz = tar::Archive::new(MultiGzDecoder::new(Cursor::new(gz)));
        let gz_offsets: Vec<(Vec<u8>, u64, u64)> = archive_gz
            .entries()
            .expect("entries gz")
            .map(|e| {
                let e = e.expect("entry");
                (e.path_bytes().to_vec(), e.raw_file_position(), e.size())
            })
            .collect();
        assert_eq!(
            gz_offsets, seek_offsets,
            "raw_file_position() sobre MultiGzDecoder da el offset DESCOMPRIMIDO correcto"
        );
    }

    /// La cancelación corta el loop en la siguiente entrada (regla 3),
    /// mismo patrón que el tar plano.
    #[test]
    fn cancelacion_corta_el_indexado() {
        let mut smith = norte_testkit::TarSmith::new();
        for i in 0..50u32 {
            smith = smith.file(format!("f{i}").as_bytes(), b"x");
        }
        let gz = gzip(&smith.build());
        let cancel = Arc::new(AtomicBool::new(true)); // armado ANTES
        let got = build_index_gz(Cursor::new(gz), (Some(0), Some(1)), &limits(), &cancel);
        assert_eq!(got.map(|_| ()).unwrap_err(), Error::Cancelled);
    }

    /// `max_decompressed_bytes` corta el pase de índice sin colgarse: fixture
    /// de "bomba" clásica (un archivo grande de ceros comprime a casi nada).
    #[test]
    fn max_decompressed_bytes_corta_la_bomba() {
        let tar = norte_testkit::TarSmith::new()
            .file(b"bomba.bin", &vec![0u8; 2_000_000])
            .build();
        let gz = gzip(&tar);
        let cancel = Arc::new(AtomicBool::new(false));
        let tight = Limits {
            max_decompressed_bytes: 1024,
            ..Limits::default()
        };
        let got = build_index_gz(Cursor::new(gz), (Some(0), Some(1)), &tight, &cancel);
        assert_eq!(got.map(|_| ()).unwrap_err(), Error::Corrupt);
    }
}
