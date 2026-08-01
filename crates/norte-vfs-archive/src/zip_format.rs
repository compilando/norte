//! Indexado y lectura de zip sobre el parser PROPIO del central directory
//! ([`zip_cd`](crate::zip_cd), #59). SYNC: corre en `spawn_blocking` sobre
//! un [`ProviderReader`](crate::blocking::ProviderReader).
//!
//! Nombres: bytes crudos del CD SIEMPRE (regla 1). El bit 11 (UTF-8) no se
//! usa para decodificar nada y el extra 0x7075 se ignora por diseño; la
//! reinterpretación manual de display es feature futura (issue de fase 8g).
//! El locator de una entrada es AUTOCONTENIDO (`Locator::Zip`): la lectura
//! resuelve el offset de datos desde el LOCAL header y descomprime con
//! flate2 — sin objeto de archive retenido ni re-parse del CD.

use std::io::{Read, Seek, SeekFrom};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use norte_proto::{EntryKind, Error};

use crate::index::{ArchiveIndex, Limits, Locator, Node};
use crate::zip_cd;

/// Construye el índice recorriendo el central directory en streaming
/// (jamás materializado, #59). `cancel` se chequea por entrada (regla 3).
/// La cuenta del EOCD/EOCD64 corta ANTES de pagar el CD si supera
/// `max_entries` (#95.3: `LimitExceeded`, no `Corrupt` — puede ser un EOCD
/// mentiroso O un zip legítimo enorme; se rehúsa a averiguarlo). Un EOCD
/// que miente a la baja también corta: las entradas REALES cuentan durante
/// el walk.
pub(crate) fn build_index<R: Read + Seek>(
    mut reader: R,
    container_len: u64,
    generation: (Option<i64>, Option<u64>),
    limits: &Limits,
    cancel: &Arc<AtomicBool>,
) -> Result<ArchiveIndex, Error> {
    let eocd = zip_cd::locate_eocd(&mut reader, container_len)?;
    if eocd.count > limits.max_entries as u64 {
        tracing::warn!(
            claimed = eocd.count,
            max = limits.max_entries,
            "EOCD supera max_entries"
        );
        return Err(Error::LimitExceeded {
            limit: Error::LIMIT_ENTRIES.into(),
        });
    }
    let mut index = ArchiveIndex::new(generation);
    let stats = zip_cd::parse_cd(&mut reader, &eocd, cancel, |entry| {
        // Kind desde los BYTES crudos, jamás desde metadatos decodificados:
        // solo `/` final es dir (`\` final es un file legal — H2).
        let node = if entry.name_raw.last() == Some(&b'/') {
            Node::dir(entry.mtime_ms)
        } else {
            let readable = entry.flags & 1 == 0 && (entry.method == 0 || entry.method == 8);
            Node {
                kind: EntryKind::File,
                size: Some(entry.uncomp_size),
                mtime_ms: entry.mtime_ms,
                // Sin locator: se LISTA (metadatos) pero read → Unsupported
                // (cifrado o método fuera de stored/deflate, ADR 0018).
                locator: readable.then_some(Locator::Zip {
                    header_offset: entry.header_offset,
                    method: entry.method,
                    crc32: entry.crc32,
                    comp_size: entry.comp_size,
                    uncomp_size: entry.uncomp_size,
                }),
                link_target: None,
                // SIEMPRE, también sin locator (#108 bloque 2): method es
                // precisamente más interesante en cifradas/no-soportadas.
                zip: Some(crate::index::ZipExtra {
                    method: entry.method,
                    crc32: entry.crc32,
                    comp_size: entry.comp_size,
                }),
            }
        };
        index.insert_entry(&entry.name_raw, node, limits)?;
        if index.skipped > limits.max_entries as u64 {
            tracing::warn!(
                max = limits.max_entries,
                "zip supera el presupuesto de omitidas"
            );
            // #95.3: mismo criterio que tar/targz — el presupuesto de
            // omitidas es un límite LOCAL, no corrupción.
            return Err(Error::LimitExceeded {
                limit: Error::LIMIT_ENTRIES.into(),
            });
        }
        Ok(())
    })?;
    index.skipped += stats.hostile_skipped;
    if eocd.count != stats.parsed {
        // Un EOCD que miente a la ALTA es metadato rancio, no fatal: las
        // entradas que anuncia y no existen cuentan como omitidas (señal).
        // A la baja ya lo cubrió el presupuesto durante el walk. Nota #59:
        // el colapso lossy del crate `zip` (H1) ya no puede ocurrir — esta
        // divergencia solo puede venir del propio EOCD.
        tracing::warn!(
            claimed = eocd.count,
            parsed = stats.parsed,
            "la cuenta del EOCD no coincide con el central directory"
        );
        index.skipped += eocd.count.saturating_sub(stats.parsed);
    }
    // rust MINOR-1 (#59 review): las hostiles del parser (zip64 malformado)
    // y el delta del EOCD entran DESPUÉS del walk — el presupuesto de
    // omitidas se re-aplica aquí, mismo criterio que dentro del walk.
    if index.skipped > limits.max_entries as u64 {
        tracing::warn!(
            max = limits.max_entries,
            "zip supera el presupuesto de omitidas (post-walk)"
        );
        return Err(Error::LimitExceeded {
            limit: Error::LIMIT_ENTRIES.into(),
        });
    }
    if index.skipped > 0 {
        tracing::warn!(
            skipped = index.skipped,
            "entradas omitidas del índice (nombres hostiles/límites); detalle en debug"
        );
    }
    Ok(index)
}

/// Parámetros de una lectura zip (#59): el locator autocontenido más el
/// recorte del range que el caller YA aplicó sobre bytes descomprimidos.
pub(crate) struct ReadPlan {
    /// Offset del LOCAL header en el contenedor.
    pub header_offset: u64,
    /// Método de compresión (0 stored / 8 deflate — el locator solo existe
    /// para esos dos).
    pub method: u16,
    /// CRC-32 que el CD declara (verificado SOLO en lecturas completas).
    pub crc32: u32,
    /// Tamaño comprimido (acota el `Take` del decoder).
    pub comp_size: u64,
    /// Tamaño descomprimido que el CD promete.
    pub uncomp_size: u64,
    /// Tamaño del contenedor (misma generación que el índice).
    pub container_len: u64,
    /// Bytes descomprimidos a saltar (range del caller).
    pub skip: u64,
    /// Bytes descomprimidos a entregar (range del caller, ya recortado
    /// contra el tamaño de la entrada).
    pub take: u64,
}

/// Lee una entrada hacia `tx` resolviendo el offset de datos desde el LOCAL
/// header — sin archive retenido ni re-parse del CD (#59). stored va con
/// seek directo; deflate descomprime en streaming (el range se aplica sobre
/// los bytes DESCOMPRIMIDOS vía skip/take). En lecturas COMPLETAS (el
/// camino de copia) el CRC del CD se verifica sobre los bytes servidos: un
/// mismatch cierra el stream con `Err(Corrupt)` como último item. Un range
/// parcial NO se verifica (documentado: exigiría descomprimir la entrada
/// entera). Si el receptor muere (drop del stream = cancelación, regla 3),
/// `blocking_send` falla y el hilo termina en el siguiente chunk.
pub(crate) fn read_entry<R: Read + Seek>(
    mut reader: R,
    plan: &ReadPlan,
    tx: &tokio::sync::mpsc::Sender<Result<bytes::Bytes, Error>>,
) {
    let data = match zip_cd::data_offset(&mut reader, plan.header_offset, plan.container_len) {
        Ok(o) => o,
        Err(e) => {
            let _ = tx.blocking_send(Err(e));
            return;
        }
    };
    // CRC solo en lecturas completas: skip==0 y take==tamaño de la entrada.
    let crc = (plan.skip == 0 && plan.take == plan.uncomp_size).then(flate2::Crc::new);
    match plan.method {
        0 => {
            // stored: APPNOTE exige comp == uncomp. Un CD que miente
            // (`uncomp > comp`) haría que un RANGED read sirviera bytes
            // vecinos del contenedor en silencio (encoding MAJOR-1 del
            // review #59 — el crate viejo acotaba por comp_size): rechazo
            // fail-loud, jamás datos ajenos atribuidos a la entrada.
            if plan.comp_size != plan.uncomp_size {
                tracing::warn!(
                    comp = plan.comp_size,
                    uncomp = plan.uncomp_size,
                    "entrada stored con tamaños inconsistentes en el CD"
                );
                let _ = tx.blocking_send(Err(Error::Corrupt));
                return;
            }
            // Bytes tal cual en el contenedor — seek directo al tramo
            // pedido, sin fase de descarte.
            let available = plan.uncomp_size.saturating_sub(plan.skip).min(plan.take);
            let Some(start) = data.checked_add(plan.skip) else {
                let _ = tx.blocking_send(Err(Error::Corrupt));
                return;
            };
            if let Err(e) = reader.seek(SeekFrom::Start(start)) {
                let _ = tx.blocking_send(Err(zip_cd::corrupt_io(&e)));
                return;
            }
            pump(&mut reader, 0, available, crc, plan.crc32, tx);
        }
        8 => {
            if let Err(e) = reader.seek(SeekFrom::Start(data)) {
                let _ = tx.blocking_send(Err(zip_cd::corrupt_io(&e)));
                return;
            }
            // El Take acota el decoder al tramo comprimido de ESTA entrada:
            // un deflate mentiroso no puede arrastrar bytes de la siguiente.
            let mut decoder = flate2::read::DeflateDecoder::new(reader.take(plan.comp_size));
            pump(&mut decoder, plan.skip, plan.take, crc, plan.crc32, tx);
        }
        other => {
            // Inalcanzable con locators del índice (readable ⇒ 0|8):
            // defensivo, jamás panic.
            tracing::warn!(method = other, "método zip sin soporte en read");
            let _ = tx.blocking_send(Err(Error::Unsupported));
        }
    }
}

/// Bombea `take` bytes (tras descartar `to_skip`) de `src` al canal en
/// chunks de 64 KiB. `Ok(0)` en CUALQUIERA de las dos fases → `Corrupt`
/// (#95.4: el caller ya recortó el range contra el tamaño de la entrada —
/// un EOF aquí solo puede ser contenedor truncado/mutado bajo nuestros
/// pies, jamás datos cortos en silencio). Con `crc` activo (lectura
/// completa) el mismatch final se envía como ÚLTIMO item `Err(Corrupt)`.
fn pump<R: Read>(
    src: &mut R,
    to_skip: u64,
    take: u64,
    mut crc: Option<flate2::Crc>,
    expected_crc: u32,
    tx: &tokio::sync::mpsc::Sender<Result<bytes::Bytes, Error>>,
) {
    let send_err = |tx: &tokio::sync::mpsc::Sender<Result<bytes::Bytes, Error>>, e: Error| {
        // Mejor esfuerzo: si el receptor murió, no hay a quién contárselo.
        let _ = tx.blocking_send(Err(e));
    };
    let mut buf = vec![0u8; 64 * 1024];
    let mut to_skip = to_skip;
    while to_skip > 0 {
        // La fase de DESCARTE no envía nada al canal: sin este chequeo, un
        // caller que dropea el stream a mitad de un skip profundo (deflate
        // ranged sobre una entrada zip64) dejaría el hilo blocking clavado
        // descomprimiendo para nadie (rust MAJOR-1 del review #59; regla 3).
        if tx.is_closed() {
            tracing::debug!("lectura zip cancelada durante el descarte (receptor muerto)");
            return;
        }
        let want = buf.len().min(usize::try_from(to_skip).unwrap_or(buf.len()));
        match src.read(&mut buf[..want]) {
            Ok(0) => return send_err(tx, Error::Corrupt),
            Ok(n) => to_skip -= n as u64,
            Err(e) => return send_err(tx, zip_cd::corrupt_io(&e)),
        }
    }
    let mut remaining = take;
    while remaining > 0 {
        let want = buf
            .len()
            .min(usize::try_from(remaining).unwrap_or(buf.len()));
        match src.read(&mut buf[..want]) {
            // Premature EOF a mitad de la entrada: el índice prometió
            // `size` bytes y no están — datos cortos JAMÁS en silencio.
            Ok(0) => return send_err(tx, Error::Corrupt),
            Ok(n) => {
                remaining -= n as u64;
                if let Some(crc) = crc.as_mut() {
                    crc.update(&buf[..n]);
                }
                if tx
                    .blocking_send(Ok(bytes::Bytes::copy_from_slice(&buf[..n])))
                    .is_err()
                {
                    tracing::debug!("lectura zip cancelada (receptor muerto)");
                    return;
                }
            }
            Err(e) => return send_err(tx, zip_cd::corrupt_io(&e)),
        }
    }
    if let Some(crc) = crc
        && crc.sum() != expected_crc
    {
        tracing::warn!("CRC del CD no coincide con los bytes servidos");
        send_err(tx, Error::Corrupt);
    }
}
