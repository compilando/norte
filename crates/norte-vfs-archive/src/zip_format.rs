//! Indexado y lectura de zip (crate `zip`). SYNC: corre en `spawn_blocking`
//! sobre un [`ProviderReader`](crate::blocking::ProviderReader).
//!
//! Nombres: `name_raw()` — bytes crudos SIEMPRE (regla 1). El bit 11 (UTF-8)
//! no se usa para decodificar nada; la reinterpretación manual de display es
//! feature futura (issue de fase 8g).

use std::io::{Read, Seek, SeekFrom};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use norte_proto::{EntryKind, Error};

use crate::index::{ArchiveIndex, Limits, Locator, Node};

/// Época civil → días desde 1970-01-01 (algoritmo de Howard Hinnant).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `DateTime` DOS del zip → ms desde epoch. El DOS time no lleva zona: se
/// interpreta como UTC (aproximación documentada; issue de deuda 8g).
fn dos_to_ms(dt: zip::DateTime) -> Option<i64> {
    let days = days_from_civil(
        i64::from(dt.year()),
        i64::from(dt.month()),
        i64::from(dt.day()),
    );
    let secs = days * 86_400
        + i64::from(dt.hour()) * 3_600
        + i64::from(dt.minute()) * 60
        + i64::from(dt.second());
    secs.checked_mul(1000)
}

/// Preflight barato ANTES de `ZipArchive::new`: cuenta de entradas Y tamaño
/// del central directory, ambos leídos del EOCD (el propio `new()`
/// materializa el CD entero — una bomba de índice O de memoria retenida
/// (#61 MAJOR-2) hay que cortarla antes de pagarla). `None` = sin preflight
/// (zip64, EOCD no localizable, firma solo en el comentario…): es una
/// optimización, el `len()` post-parse sigue cortando bombas de entradas y
/// `ZipArchive::new` decide qué es corrupto; para el tope de bytes cacheados
/// (`max_cd_bytes`) un preflight desconocido se trata fail-closed (no se
/// cachea) en `build_index`.
fn eocd_preflight<R: Read + Seek>(reader: &mut R, len: u64) -> Option<(u64, u64)> {
    const EOCD_SIG: [u8; 4] = [0x50, 0x4b, 0x05, 0x06];
    // EOCD = 22 bytes + comentario ≤ 65535: la firma vive en la última ventana.
    let window = 22u64 + 65_535;
    let start = len.saturating_sub(window);
    let take = usize::try_from(len - start).ok()?;
    reader.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = vec![0u8; take];
    reader.read_exact(&mut buf).ok()?;
    // Un comentario puede CONTENER la firma (H9): el candidato solo vale si
    // su cd_offset+cd_size apunta exactamente a su propia posición. Si no,
    // sigue buscando hacia atrás.
    let mut search = buf.len();
    while let Some(pos) = buf[..search].windows(4).rposition(|w| w == EOCD_SIG) {
        search = pos;
        if pos + 22 > buf.len() {
            continue;
        }
        let count = u16::from_le_bytes([buf[pos + 10], buf[pos + 11]]);
        let cd_size = u32::from_le_bytes(buf[pos + 12..pos + 16].try_into().ok()?);
        let cd_off = u32::from_le_bytes(buf[pos + 16..pos + 20].try_into().ok()?);
        if count == u16::MAX || cd_off == u32::MAX || cd_size == u32::MAX {
            return None; // zip64: el contador/tamaño real vive en el EOCD64
        }
        if u64::from(cd_off) + u64::from(cd_size) == start + pos as u64 {
            return Some((u64::from(count), u64::from(cd_size)));
        }
    }
    None
}

/// Construye el índice desde el central directory (`by_index_raw`: nunca
/// descomprime). `cancel` se chequea por entrada (regla 3). Devuelve también
/// el `ZipArchive` ya parseado (#61) SI su central directory cabe bajo
/// `limits.max_cd_bytes` — por encima (o si el preflight no pudo medirlo,
/// fail-closed) el índice se construye igual pero `None`: el caller no lo
/// cachea y un `read` posterior vuelve a parsear (comportamiento pre-caché,
/// #61 MAJOR-2 — el tope gobierna memoria RETENIDA, no el indexado).
pub(crate) fn build_index<R: Read + Seek>(
    mut reader: R,
    container_len: u64,
    generation: (Option<i64>, Option<u64>),
    limits: &Limits,
    cancel: &Arc<AtomicBool>,
) -> Result<(ArchiveIndex, Option<zip::ZipArchive<R>>), Error> {
    let preflight = eocd_preflight(&mut reader, container_len);
    let claimed = preflight.map(|(count, _)| count);
    if let Some(claimed) = claimed
        && claimed > limits.max_entries as u64
    {
        tracing::warn!(claimed, max = limits.max_entries, "EOCD supera max_entries");
        return Err(Error::Corrupt);
    }
    let mut archive = zip::ZipArchive::new(reader).map_err(|e| corrupt(&e))?;
    if archive.len() > limits.max_entries {
        // zip64 (el preflight u16 no lo cubre) o EOCD mentiroso a la baja.
        tracing::warn!(
            entries = archive.len(),
            max = limits.max_entries,
            "zip supera max_entries"
        );
        return Err(Error::Corrupt);
    }
    let mut index = ArchiveIndex::new(generation);
    // Mitigación H1 (auditoría 8e): zip 5.x indexa el central directory por
    // el nombre DECODIFICADO (lossy) — dos nombres crudos distintos que
    // decodifican igual COLAPSAN antes de que norte los vea (y el último
    // gana: shadowing). En no-zip64 el colapso se detecta comparando el
    // contador del EOCD con lo materializado; fix real = parser propio del
    // CD (issue upstream, deuda 8g).
    if let Some(claimed) = claimed
        && claimed != archive.len() as u64
    {
        let lost = claimed.saturating_sub(archive.len() as u64);
        index.skipped += lost;
        tracing::warn!(
            claimed,
            materialized = archive.len(),
            "el crate zip colapsó entradas por decode lossy de nombres (H1)"
        );
    }
    for i in 0..archive.len() {
        if cancel.load(Ordering::Relaxed) {
            tracing::debug!("indexado zip cancelado");
            return Err(Error::Cancelled);
        }
        let file = archive.by_index_raw(i).map_err(|e| corrupt(&e))?;
        let raw_name = file.name_raw().to_vec();
        let mtime_ms = file.last_modified().and_then(dos_to_ms);
        // Kind desde los BYTES, jamás desde `file.is_dir()`: el crate zip lo
        // decide sobre el nombre DECODIFICADO y trata `\` final como dir —
        // un file legal `trailing\` quedaría ilegible (auditoría 8e, H2).
        let node = if raw_name.last() == Some(&b'/') {
            Node::dir(mtime_ms)
        } else {
            let readable = !file.encrypted()
                && matches!(
                    file.compression(),
                    zip::CompressionMethod::Stored | zip::CompressionMethod::Deflated
                );
            Node {
                kind: EntryKind::File,
                size: Some(file.size()),
                mtime_ms,
                // Sin locator: se LISTA (metadatos) pero read → Unsupported
                // (cifrado o método fuera de stored/deflate, ADR 0018).
                locator: readable.then_some(Locator::Zip { index: i }),
                link_target: None,
            }
        };
        drop(file);
        index.insert_entry(&raw_name, node, limits)?;
        if index.skipped > limits.max_entries as u64 {
            tracing::warn!(
                max = limits.max_entries,
                "zip supera el presupuesto de omitidas"
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
    // MAJOR-2 (#61): el CD parseado solo se cachea si cabe bajo el tope de
    // memoria retenida. `cd_size` desconocido (preflight None: zip64, EOCD
    // no localizable…) se trata como "excede" — fail-closed, ya que no hay
    // forma de verificar el presupuesto.
    let cd_size = preflight.map(|(_, size)| size);
    let cacheable = cd_size.is_some_and(|size| size <= limits.max_cd_bytes);
    if !cacheable {
        tracing::debug!(
            ?cd_size,
            max_cd_bytes = limits.max_cd_bytes,
            "central directory no cacheado (tope de memoria retenida, #61)"
        );
    }
    Ok((index, cacheable.then_some(archive)))
}

/// Abre el archive en el hilo blocking; si el CD está roto, reporta por el
/// canal y devuelve `None` (el caller retorna). Camino frío de `read`: sin
/// `ZipArchive` cacheado (generación desconocida, MINOR-4; o CD por encima
/// de `max_cd_bytes`, MAJOR-2 — ambos #61).
pub(crate) fn open_archive<R: Read + Seek>(
    reader: R,
    tx: &tokio::sync::mpsc::Sender<Result<bytes::Bytes, Error>>,
) -> Option<zip::ZipArchive<R>> {
    match zip::ZipArchive::new(reader) {
        Ok(a) => Some(a),
        Err(e) => {
            let _ = tx.blocking_send(Err(corrupt(&e)));
            None
        }
    }
}

/// Lee la entrada `index` descomprimiendo en streaming hacia `tx`. El caller
/// aplica el range SOBRE los bytes descomprimidos vía `skip`/`take`. Si el
/// receptor muere (drop del stream = cancelación, regla 3), `blocking_send`
/// falla y el hilo termina en el siguiente chunk.
///
/// `archive` viene YA PARSEADO (#61): del cache (clon barato, CD compartido
/// por Arc interno) o del camino frío vía [`open_archive`] — nunca se
/// reconstruye aquí.
pub(crate) fn read_entry<R: Read + Seek>(
    mut archive: zip::ZipArchive<R>,
    entry_index: usize,
    skip: u64,
    take: u64,
    tx: &tokio::sync::mpsc::Sender<Result<bytes::Bytes, Error>>,
) {
    let send_err = |tx: &tokio::sync::mpsc::Sender<Result<bytes::Bytes, Error>>, e: Error| {
        // Mejor esfuerzo: si el receptor murió, no hay a quién contárselo.
        let _ = tx.blocking_send(Err(e));
    };
    let mut file = match archive.by_index(entry_index) {
        Ok(f) => f,
        Err(e) => return send_err(tx, corrupt(&e)),
    };
    // Saltar `skip` bytes descomprimidos (deflate no tiene seek).
    let mut to_skip = skip;
    let mut buf = vec![0u8; 64 * 1024];
    while to_skip > 0 {
        let want = buf.len().min(usize::try_from(to_skip).unwrap_or(buf.len()));
        match file.read(&mut buf[..want]) {
            Ok(0) => return, // EOF antes del offset: stream vacío (pread)
            Ok(n) => to_skip -= n as u64,
            Err(e) => return send_err(tx, corrupt_io(&e)),
        }
    }
    let mut remaining = take;
    while remaining > 0 {
        let want = buf
            .len()
            .min(usize::try_from(remaining).unwrap_or(buf.len()));
        match file.read(&mut buf[..want]) {
            Ok(0) => return,
            Ok(n) => {
                remaining -= n as u64;
                if tx
                    .blocking_send(Ok(bytes::Bytes::copy_from_slice(&buf[..n])))
                    .is_err()
                {
                    tracing::debug!("lectura zip cancelada (receptor muerto)");
                    return;
                }
            }
            Err(e) => return send_err(tx, corrupt_io(&e)),
        }
    }
}

fn corrupt(e: &zip::result::ZipError) -> Error {
    // El brazo Io puede envolver un fallo del provider interior: delega en
    // el mismo criterio que `corrupt_io` (#58).
    if let zip::result::ZipError::Io(io) = e {
        return corrupt_io(io);
    }
    tracing::warn!(error = %e, "zip corrupto o ilegible");
    Error::Corrupt
}

fn corrupt_io(e: &std::io::Error) -> Error {
    // IO genuino del provider interior (corte de red a mitad de parseo):
    // se propaga VERBATIM, jamás se disfraza de Corrupt (#58).
    if let Some(inner) = crate::blocking::inner_proto_error(e) {
        return inner;
    }
    tracing::warn!(error = %e, "error de IO leyendo entrada zip");
    Error::Corrupt
}
