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
            // #95.3: bomba O backup legítimo enorme — límite local honesto.
            // `inner_proto_error` lo desenvuelve de la cadena io::Error.
            return Err(std::io::Error::other(Error::LimitExceeded {
                limit: "decompressed-bytes".into(),
            }));
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
            return Err(Error::LimitExceeded {
                limit: "entries".into(),
            });
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
/// canal acotado.
///
/// EOF prematuro es FAIL-LOUD en AMBAS fases, `skip` Y `take` (fix de
/// review #55: la fase de descarte devolvía silenciosamente un stream vacío
/// — INCORRECTO). El caller (`ArchiveProvider::read`) ya recortó `req_off`
/// contra `entry_size` ANTES de lanzar este hilo (semántica pread): un EOF
/// aquí NUNCA es "offset legítimamente fuera de la entrada" (eso ya lo
/// filtró el caller) — solo puede significar contenedor truncado o mutado
/// bajo nuestros pies (el mismo evento que documenta
/// [`ProviderReader::read`](crate::blocking::ProviderReader)), jamás datos
/// cortos en silencio.
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
            Ok(0) => {
                // FIX-1 (rust+security MAJOR, #55 review): el caller YA
                // recortó `req_off` contra `entry_size` — un EOF aquí solo
                // puede ser contenedor truncado/mutado bajo nuestros pies,
                // jamás un offset legítimamente vacío. Fail-loud, igual que
                // el EOF prematuro de la fase `take`.
                return send_err(tx, Error::Corrupt);
            }
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
        assert_eq!(
            got.map(|_| ()).unwrap_err(),
            Error::LimitExceeded {
                limit: "decompressed-bytes".into()
            }
        );
    }

    /// FIX-1 (rust+security MAJOR, #55 review): EOF durante el DESCARTE
    /// (`skip`) debe ser fail-loud, no un stream vacío silencioso. `skip`
    /// aquí supera lo que el gz truncado puede entregar — antes del fix esto
    /// devolvía Ok(()) sin ningún mensaje por el canal (indistinguible de
    /// "no hay más datos porque el receptor cerró"); ahora debe llegar
    /// exactamente UN mensaje `Err(Corrupt)`.
    #[test]
    fn eof_durante_el_descarte_es_corrupt_no_vacio() {
        let tar = norte_testkit::TarSmith::new()
            .file(b"grande.bin", &[7u8; 4000])
            .build();
        let gz = gzip(&tar);
        // Corta el gz a la mitad: el decoder no puede entregar los 4000
        // bytes descomprimidos que `skip` pide.
        let truncated = gz[..gz.len() / 2].to_vec();

        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        read_entry_gz(Cursor::new(truncated), 3_900, 10, &tx);
        drop(tx);

        match rx.blocking_recv() {
            Some(Err(Error::Corrupt)) => {}
            other => panic!(
                "esperaba EXACTAMENTE un Err(Corrupt) por EOF durante el descarte, fue {other:?}"
            ),
        }
        assert!(
            rx.blocking_recv().is_none(),
            "ni un byte de datos tras el EOF prematuro: jamás cortos en silencio"
        );
    }

    /// Reader que arma `cancel` (el MISMO que recibe `build_index_gz`) tras
    /// servir sus primeros `arm_after` bytes CRUDOS (comprimidos): simula
    /// una cancelación real EN MEDIO del pipeline de descompresión —
    /// distinto del test `cancelacion_corta_el_indexado` de arriba, que
    /// arma el flag ANTES de arrancar (corta en la PRIMERISIMA lectura, sin
    /// que el build haya progresado nada todavía). FIX-4 (rust MINOR-3a,
    /// #55 review).
    struct ArmCancelAfter<R> {
        inner: R,
        served: u64,
        arm_after: u64,
        cancel: Arc<AtomicBool>,
    }

    impl<R: Read> Read for ArmCancelAfter<R> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = self.inner.read(buf)?;
            self.served += n as u64;
            if self.served >= self.arm_after {
                self.cancel.store(true, Ordering::Relaxed);
            }
            Ok(n)
        }
    }

    /// FIX-4 (rust MINOR-3a, #55 review): cancelación DESPUÉS de que el
    /// pipeline ya sirvió bytes reales (no antes de que arranque el build) —
    /// el resultado sigue siendo `Cancelled`, NUNCA `Corrupt`. También
    /// valida la cadena `source()` de FIX-3: la señal atraviesa flate2 +
    /// tar-rs sin perder su identidad, aunque quede reenvuelta por el
    /// camino.
    #[test]
    fn cancelacion_a_mitad_del_pipeline_es_cancelled_no_corrupt() {
        // Contenido de ALTA entropía (xorshift32, no un patrón periódico):
        // deflate no puede comprimir ruido genuino, así que el gz resultante
        // es ~proporcional al tamaño descomprimido — evita que TODO el gz
        // quepa en un solo buffer interno de flate2 (lo que dejaría
        // `served` saltar de 0 al total en una sola lectura y perdería el
        // matiz "a mitad").
        let mut state: u32 = 0x2545_F491;
        let contenido: Vec<u8> = (0..2_000_000u32)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                (state & 0xFF) as u8
            })
            .collect();
        let tar = norte_testkit::TarSmith::new()
            .file(b"grande.bin", &contenido)
            .file(b"segunda.bin", b"x")
            .build();
        let gz = gzip(&tar);
        let gz_len = gz.len() as u64;
        assert!(
            gz_len > 100_000,
            "contenido poco compresible: el gz debe seguir siendo grande"
        );

        let cancel = Arc::new(AtomicBool::new(false));
        let reader = ArmCancelAfter {
            inner: Cursor::new(gz),
            served: 0,
            arm_after: gz_len / 2, // a mitad del stream comprimido
            cancel: Arc::clone(&cancel),
        };
        let got = build_index_gz(reader, (Some(0), Some(1)), &limits(), &cancel);
        assert_eq!(
            got.map(|_| ()).unwrap_err(),
            Error::Cancelled,
            "cancelación a mitad del pipeline: Cancelled, NO Corrupt"
        );
    }
}
