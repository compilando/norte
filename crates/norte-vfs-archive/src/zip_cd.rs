//! Parser PROPIO del central directory de zip (#59): EOCD/EOCD64 +
//! recorrido en streaming del CD + resolución del offset de datos desde el
//! LOCAL header. SYNC: corre en `spawn_blocking` sobre un
//! [`ProviderReader`](crate::blocking::ProviderReader).
//!
//! Reemplaza al crate `zip` en el camino de indexado/lectura:
//! - Los nombres son los bytes CRUDOS del CD, verbatim (regla 1) — sin el
//!   colapso lossy de nombres que decodifican igual (H1).
//! - El extra 0x7075 (Info-ZIP unicode path) se IGNORA POR DISEÑO: jamás
//!   sustituye el nombre ni mata el archivo (H3).
//! - El CD JAMÁS se materializa entero: se recorre entrada a entrada bajo
//!   un `Take` (`Limits::max_cd_bytes` queda obsoleto — no hay memoria
//!   retenida que gobernar).
//! - zip64: los marcadores del EOCD llevan al EOCD64 vía su locator; la
//!   cuenta real de 64 bits entra al preflight de `max_entries` (el hueco
//!   del preflight u16 queda cerrado).

use std::io::{Read, Seek, SeekFrom};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use norte_proto::Error;

const EOCD_SIG: [u8; 4] = [0x50, 0x4b, 0x05, 0x06];
const EOCD64_SIG: [u8; 4] = [0x50, 0x4b, 0x06, 0x06];
const EOCD64_LOCATOR_SIG: [u8; 4] = [0x50, 0x4b, 0x06, 0x07];
const CD_ENTRY_SIG: [u8; 4] = [0x50, 0x4b, 0x01, 0x02];
const LOCAL_SIG: [u8; 4] = [0x50, 0x4b, 0x03, 0x04];

/// End of central directory ya resuelto (clásico o zip64): lo que el
/// indexado necesita para el preflight de entradas y el walk del CD.
pub(crate) struct Eocd {
    /// Entradas que el EOCD declara (puede mentir en ambos sentidos).
    pub count: u64,
    /// Offset del central directory en el contenedor.
    pub cd_offset: u64,
    /// Bytes del central directory.
    pub cd_size: u64,
}

/// Una entrada del central directory, con los campos zip64 YA resueltos.
pub(crate) struct CdEntry {
    /// Nombre en bytes CRUDOS del CD, verbatim (regla 1).
    pub name_raw: Vec<u8>,
    /// General purpose flags (bit 0 = cifrado).
    pub flags: u16,
    /// Método de compresión (0 stored / 8 deflate / otros).
    pub method: u16,
    /// CRC-32 declarado de los bytes SIN comprimir.
    pub crc32: u32,
    /// Tamaño comprimido.
    pub comp_size: u64,
    /// Tamaño sin comprimir.
    pub uncomp_size: u64,
    /// Offset del LOCAL header en el contenedor.
    pub header_offset: u64,
    /// mtime en ms desde epoch (DOS time interpretado como UTC), si el par
    /// DOS es válido.
    pub mtime_ms: Option<i64>,
}

/// Resultado del walk del CD: cuántas entradas se consumieron y cuántas de
/// ellas eran hostiles (omitidas SIN entregarse a `per_entry`). `parsed`
/// INCLUYE las hostiles: la comparación contra `Eocd::count` va sobre lo
/// realmente consumido del CD.
pub(crate) struct ParseStats {
    /// Entradas consumidas del CD (entregadas + hostiles).
    pub parsed: u64,
    /// Subconjunto omitido por extra zip64 malformado (marcador sin valor).
    pub hostile_skipped: u64,
}

/// u16 LE en `off`. Invariante del caller: `off + 2 <= b.len()` (offsets
/// constantes sobre buffers de tamaño fijo o pre-chequeados).
fn le16(b: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([b[off], b[off + 1]])
}

/// u32 LE en `off`. Invariante del caller: `off + 4 <= b.len()`.
fn le32(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(
        b[off..off + 4]
            .try_into()
            .expect("rango constante dentro del buffer"),
    )
}

/// u64 LE en `off`. Invariante del caller: `off + 8 <= b.len()`.
fn le64(b: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(
        b[off..off + 8]
            .try_into()
            .expect("rango constante dentro del buffer"),
    )
}

/// Localiza el EOCD escaneando hacia atrás la última ventana (22 bytes +
/// comentario máximo de 65535). Un comentario puede CONTENER la firma (H9):
/// un candidato solo vale si es autoconsistente — `cd_offset + cd_size`
/// apunta exactamente a su posición — o si su rama zip64 (marcadores →
/// locator → EOCD64) es consistente. Sin EOCD localizable → `Corrupt`.
pub(crate) fn locate_eocd<R: Read + Seek>(
    reader: &mut R,
    container_len: u64,
) -> Result<Eocd, Error> {
    let window = 22u64 + 65_535;
    let start = container_len.saturating_sub(window);
    let take = usize::try_from(container_len - start).map_err(|_| Error::Corrupt)?;
    reader
        .seek(SeekFrom::Start(start))
        .map_err(|e| corrupt_io(&e))?;
    let mut buf = vec![0u8; take];
    reader.read_exact(&mut buf).map_err(|e| corrupt_io(&e))?;
    let mut search = buf.len();
    while let Some(pos) = buf[..search].windows(4).rposition(|w| w == EOCD_SIG) {
        search = pos;
        if pos + 22 > buf.len() {
            continue;
        }
        let count = le16(&buf, pos + 10);
        let cd_size = le32(&buf, pos + 12);
        let cd_off = le32(&buf, pos + 16);
        let candidate_pos = start + pos as u64;
        if count == u16::MAX || cd_off == u32::MAX || cd_size == u32::MAX {
            // Marcadores zip64: cuenta/tamaño/offset reales en el EOCD64,
            // localizado por el record de 20 bytes JUSTO antes del EOCD.
            if let Some(eocd) = read_zip64(reader, candidate_pos)? {
                return Ok(eocd);
            }
            continue; // marcador sin cadena zip64 consistente: sigue atrás
        }
        if u64::from(cd_off) + u64::from(cd_size) == candidate_pos {
            return Ok(Eocd {
                count: u64::from(count),
                cd_offset: u64::from(cd_off),
                cd_size: u64::from(cd_size),
            });
        }
    }
    tracing::warn!("sin EOCD localizable: no es un zip");
    Err(Error::Corrupt)
}

/// `read_exact` que distingue el EOF estructural (`Ok(false)`: el candidato
/// no es zip64, el caller sigue buscando) del IO genuino del provider
/// interior (verbatim, #58).
fn read_exact_or_eof<R: Read>(reader: &mut R, buf: &mut [u8]) -> Result<bool, Error> {
    match reader.read_exact(buf) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => Ok(false),
        Err(e) => Err(corrupt_io(&e)),
    }
}

/// La rama zip64 de un candidato a EOCD con marcadores: locator de 20 bytes
/// justo antes del EOCD → offset del EOCD64 → `count`/`cd_size`/`cd_offset` de 64
/// bits. `Ok(None)` = estructura no consistente (el caller sigue buscando
/// hacia atrás); `Err` solo para IO genuino del interior.
fn read_zip64<R: Read + Seek>(reader: &mut R, eocd_pos: u64) -> Result<Option<Eocd>, Error> {
    let Some(locator_pos) = eocd_pos.checked_sub(20) else {
        return Ok(None);
    };
    reader
        .seek(SeekFrom::Start(locator_pos))
        .map_err(|e| corrupt_io(&e))?;
    let mut locator = [0u8; 20];
    if !read_exact_or_eof(reader, &mut locator)? {
        return Ok(None);
    }
    if locator[..4] != EOCD64_LOCATOR_SIG {
        return Ok(None);
    }
    let eocd64_pos = le64(&locator, 8);
    if eocd64_pos >= locator_pos {
        return Ok(None); // el EOCD64 debe vivir ANTES de su locator
    }
    reader
        .seek(SeekFrom::Start(eocd64_pos))
        .map_err(|e| corrupt_io(&e))?;
    let mut record = [0u8; 56];
    if !read_exact_or_eof(reader, &mut record)? {
        return Ok(None);
    }
    if record[..4] != EOCD64_SIG {
        return Ok(None);
    }
    let count = le64(&record, 32);
    let cd_size = le64(&record, 40);
    let cd_offset = le64(&record, 48);
    // Consistencia (paridad con la regla clásica): el CD termina a lo sumo
    // donde empieza el EOCD64.
    match cd_offset.checked_add(cd_size) {
        Some(end) if end <= eocd64_pos => Ok(Some(Eocd {
            count,
            cd_offset,
            cd_size,
        })),
        _ => Ok(None),
    }
}

/// Resuelve comp/uncomp/`header_offset` finales caminando el extra blob del
/// CD. `None` = entrada HOSTIL (marcador zip64 sin su valor de 64 bits): se
/// omite la ENTRADA, jamás mata el archivo. Records truncados o que
/// desbordan el blob: se deja de caminar y valen los valores del CD
/// (conservador — la entrada sobrevive). El id 0x7075 (Info-ZIP unicode
/// path) se IGNORA POR DISEÑO: jamás sustituye `name_raw` (H3).
fn resolve_extra(extra: &[u8], comp32: u32, uncomp32: u32, off32: u32) -> Option<(u64, u64, u64)> {
    let mut comp = u64::from(comp32);
    let mut uncomp = u64::from(uncomp32);
    let mut off = u64::from(off32);
    let any_marker = comp32 == u32::MAX || uncomp32 == u32::MAX || off32 == u32::MAX;
    let mut resolved = false;
    let mut pos = 0usize;
    while pos + 4 <= extra.len() {
        let id = le16(extra, pos);
        let size = usize::from(le16(extra, pos + 2));
        let end = pos + 4 + size;
        if end > extra.len() {
            break; // record truncado: valores del CD, la entrada sobrevive
        }
        if id == 0x0001 && !resolved {
            // zip64: valores u64 en orden APPNOTE — uncomp, comp, header
            // offset (el nº de disco u32 va al final, ignorado) — SOLO para
            // los campos cuyo valor del CD es el marcador 0xFFFF_FFFF.
            // Interop documentada (estilo Go archive/zip): un writer no
            // conforme que emita el triple completo marcando solo algunos
            // campos se malinterpreta — estricto a propósito.
            let mut body = &extra[pos + 4..end];
            for (needed, slot) in [
                (uncomp32 == u32::MAX, &mut uncomp),
                (comp32 == u32::MAX, &mut comp),
                (off32 == u32::MAX, &mut off),
            ] {
                if needed {
                    if body.len() < 8 {
                        return None; // marcador sin su valor: hostil
                    }
                    *slot = le64(body, 0);
                    body = &body[8..];
                }
            }
            // El PRIMER record 0x0001 manda (review #59): un segundo no
            // puede re-escribir los valores (ambigüedad hostil).
            resolved = true;
        }
        // 0x7075 y demás ids: ignorados (ver doc del módulo).
        pos = end;
    }
    if any_marker && !resolved {
        // Campos marcados 0xFFFF_FFFF sin NINGÚN record 0x0001: el CD
        // promete zip64 y no lo entrega — hostil (antes el literal
        // 0xFFFFFFFF se colaba como tamaño/offset mentira).
        return None;
    }
    Some((comp, uncomp, off))
}

/// Recorre el central directory en STREAMING (jamás materializado: `Take`
/// de `cd_size`) entregando cada entrada a `per_entry`; el error de
/// `per_entry` corta y se propaga. `cancel` se chequea por entrada (regla
/// 3). Estructura rota (firma inválida, CD truncado a mitad de entrada) →
/// `Corrupt`; una entrada hostil (extra zip64 con marcador sin valor) se
/// OMITE con `warn!` y cuenta en [`ParseStats::hostile_skipped`], sin matar
/// el archivo.
pub(crate) fn parse_cd<R: Read + Seek>(
    reader: &mut R,
    eocd: &Eocd,
    cancel: &Arc<AtomicBool>,
    mut per_entry: impl FnMut(CdEntry) -> Result<(), Error>,
) -> Result<ParseStats, Error> {
    reader
        .seek(SeekFrom::Start(eocd.cd_offset))
        .map_err(|e| corrupt_io(&e))?;
    let mut cd = reader.by_ref().take(eocd.cd_size);
    let mut stats = ParseStats {
        parsed: 0,
        hostile_skipped: 0,
    };
    while cd.limit() > 0 {
        if cancel.load(Ordering::Relaxed) {
            tracing::debug!("parse del central directory cancelado");
            return Err(Error::Cancelled);
        }
        let mut header = [0u8; 46];
        cd.read_exact(&mut header).map_err(|e| corrupt_io(&e))?;
        if header[..4] != CD_ENTRY_SIG {
            tracing::warn!("firma de entrada del central directory inválida");
            return Err(Error::Corrupt);
        }
        let flags = le16(&header, 8);
        let method = le16(&header, 10);
        let dos_time = le16(&header, 12);
        let dos_date = le16(&header, 14);
        let crc32 = le32(&header, 16);
        let comp32 = le32(&header, 20);
        let uncomp32 = le32(&header, 24);
        let name_len = usize::from(le16(&header, 28));
        let extra_len = usize::from(le16(&header, 30));
        let comment_len = u64::from(le16(&header, 32));
        let off32 = le32(&header, 42);
        let mut name_raw = vec![0u8; name_len];
        cd.read_exact(&mut name_raw).map_err(|e| corrupt_io(&e))?;
        let mut extra = vec![0u8; extra_len];
        cd.read_exact(&mut extra).map_err(|e| corrupt_io(&e))?;
        // El comentario se salta sin materializar; quedarse corto es un CD
        // truncado (#95.4: jamás silencio a mitad de estructura).
        let skipped = std::io::copy(&mut (&mut cd).take(comment_len), &mut std::io::sink())
            .map_err(|e| corrupt_io(&e))?;
        if skipped != comment_len {
            tracing::warn!("central directory truncado a mitad de un comentario");
            return Err(Error::Corrupt);
        }
        stats.parsed += 1;
        let Some((comp_size, uncomp_size, header_offset)) =
            resolve_extra(&extra, comp32, uncomp32, off32)
        else {
            stats.hostile_skipped += 1;
            tracing::warn!(
                name = ?String::from_utf8_lossy(&name_raw),
                "extra zip64 con marcador sin valor: entrada omitida (hostil)"
            );
            continue;
        };
        per_entry(CdEntry {
            name_raw,
            flags,
            method,
            crc32,
            comp_size,
            uncomp_size,
            header_offset,
            mtime_ms: dos_pair_to_ms(dos_time, dos_date),
        })?;
    }
    Ok(stats)
}

/// Offset del PRIMER byte de datos de una entrada: salta el LOCAL header
/// (30 bytes fijos + name/extra LOCALES — pueden diferir de las copias del
/// CD). `Corrupt` si la firma no cuadra o el offset resultante sale del
/// contenedor.
pub(crate) fn data_offset<R: Read + Seek>(
    reader: &mut R,
    header_offset: u64,
    container_len: u64,
) -> Result<u64, Error> {
    reader
        .seek(SeekFrom::Start(header_offset))
        .map_err(|e| corrupt_io(&e))?;
    let mut local = [0u8; 30];
    reader.read_exact(&mut local).map_err(|e| corrupt_io(&e))?;
    if local[..4] != LOCAL_SIG {
        tracing::warn!("firma de local header inválida");
        return Err(Error::Corrupt);
    }
    let name_len = u64::from(le16(&local, 26));
    let extra_len = u64::from(le16(&local, 28));
    let data = header_offset
        .checked_add(30 + name_len + extra_len)
        .ok_or(Error::Corrupt)?;
    if data > container_len {
        tracing::warn!("local header apunta fuera del contenedor");
        return Err(Error::Corrupt);
    }
    Ok(data)
}

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

/// Par DOS crudo `(time, date)` → ms desde epoch. El DOS time no lleva
/// zona: se interpreta como UTC (aproximación documentada; issue de deuda
/// 8g). Mes 0/>12 o día 0 → `None` (defensivo: el par viene de bytes
/// hostiles, no de un reloj).
pub(crate) fn dos_pair_to_ms(time: u16, date: u16) -> Option<i64> {
    let y = 1980 + i64::from(date >> 9);
    let m = i64::from((date >> 5) & 0xF);
    let d = i64::from(date & 0x1F);
    if m == 0 || m > 12 || d == 0 {
        return None;
    }
    let secs = days_from_civil(y, m, d) * 86_400
        + i64::from(time >> 11) * 3_600
        + i64::from((time >> 5) & 0x3F) * 60
        + i64::from(time & 0x1F) * 2;
    secs.checked_mul(1000)
}

/// IO genuino del provider interior (corte de red a mitad de parseo): se
/// propaga VERBATIM, jamás se disfraza de `Corrupt` (#58). El resto (EOF
/// prematuro, basura) sí es estructura rota.
pub(crate) fn corrupt_io(e: &std::io::Error) -> Error {
    if let Some(inner) = crate::blocking::inner_proto_error(e) {
        return inner;
    }
    tracing::warn!(error = %e, "zip corrupto o ilegible");
    Error::Corrupt
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use norte_testkit::ZipSmith;

    use super::*;

    fn no_cancel() -> Arc<AtomicBool> {
        Arc::new(AtomicBool::new(false))
    }

    fn collect_cd(bytes: &[u8]) -> (Eocd, Vec<CdEntry>, ParseStats) {
        let mut reader = Cursor::new(bytes.to_vec());
        let eocd = locate_eocd(&mut reader, bytes.len() as u64).expect("eocd");
        let mut entries = Vec::new();
        let stats = parse_cd(&mut reader, &eocd, &no_cancel(), |e| {
            entries.push(e);
            Ok(())
        })
        .expect("parse");
        (eocd, entries, stats)
    }

    #[test]
    fn eocd_con_firma_falsa_en_el_comentario() {
        // H9: el candidato del comentario no es autoconsistente — se sigue
        // hacia atrás hasta el EOCD real.
        let mut fake = b"PK\x05\x06".to_vec();
        fake.extend_from_slice(&[0u8; 16]);
        fake.extend_from_slice(&0u16.to_le_bytes());
        let bytes = ZipSmith::new()
            .file(b"real.txt", b"ok")
            .comment(&fake)
            .build();
        let (eocd, entries, _) = collect_cd(&bytes);
        assert_eq!(eocd.count, 1);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name_raw, b"real.txt");
    }

    #[test]
    fn zip64_roundtrip_del_final() {
        let bytes = ZipSmith::new().file(b"z.txt", b"abc").build_zip64();
        let (eocd, entries, stats) = collect_cd(&bytes);
        assert_eq!(eocd.count, 1);
        assert_eq!(stats.parsed, 1);
        assert_eq!(entries[0].name_raw, b"z.txt");
        assert_eq!(entries[0].uncomp_size, 3);
    }

    #[test]
    fn extra_zip64_resuelve_los_marcadores_en_orden() {
        // uncomp y offset con marcador; comp normal: el blob lleva DOS u64
        // (uncomp, offset) — orden APPNOTE saltándose comp.
        let mut extra = 0x0001u16.to_le_bytes().to_vec();
        extra.extend_from_slice(&16u16.to_le_bytes());
        extra.extend_from_slice(&77u64.to_le_bytes()); // uncomp
        extra.extend_from_slice(&99u64.to_le_bytes()); // header offset
        let got = resolve_extra(&extra, 5, u32::MAX, u32::MAX).expect("válido");
        assert_eq!(got, (5, 77, 99));
    }

    #[test]
    fn extra_zip64_marcador_sin_valor_es_hostil() {
        // uncomp marcado pero el record solo trae 4 bytes: hostil (None).
        let mut extra = 0x0001u16.to_le_bytes().to_vec();
        extra.extend_from_slice(&4u16.to_le_bytes());
        extra.extend_from_slice(&[0u8; 4]);
        assert!(resolve_extra(&extra, 0, u32::MAX, 0).is_none());
    }

    /// enc MAJOR-2 (review #59): el caso CANÓNICO — los TRES campos
    /// marcados, tres valores distintos — pinea el orden APPNOTE completo
    /// (uncomp, comp, offset). Una permutación de comp/uncomp aquí es
    /// exactamente el mutante que sobrevivía a la suite.
    #[test]
    fn extra_zip64_tres_marcadores_orden_canonico() {
        let mut extra = 0x0001u16.to_le_bytes().to_vec();
        extra.extend_from_slice(&24u16.to_le_bytes());
        extra.extend_from_slice(&111u64.to_le_bytes()); // uncomp
        extra.extend_from_slice(&222u64.to_le_bytes()); // comp
        extra.extend_from_slice(&333u64.to_le_bytes()); // header offset
        let got = resolve_extra(&extra, u32::MAX, u32::MAX, u32::MAX).expect("válido");
        assert_eq!(got, (222, 111, 333), "(comp, uncomp, off) exactos");
    }

    /// Review #59: campos MARCADOS sin ningún record 0x0001 en el extra —
    /// el CD promete zip64 y no lo entrega: hostil (antes el literal
    /// 0xFFFFFFFF se colaba como tamaño mentira de 4 GiB−1).
    #[test]
    fn extra_zip64_marcador_sin_record_es_hostil() {
        // Extra con solo un 0x7075 (ignorado): el marcador queda sin valor.
        let mut extra = 0x7075u16.to_le_bytes().to_vec();
        extra.extend_from_slice(&1u16.to_le_bytes());
        extra.push(1);
        assert!(resolve_extra(&extra, u32::MAX, 0, 0).is_none());
        // Extra VACÍO con marcador: ídem.
        assert!(resolve_extra(&[], 0, u32::MAX, 0).is_none());
    }

    /// Review #59: el PRIMER record 0x0001 manda — un segundo record no
    /// re-escribe los valores (ambigüedad hostil resuelta conservadora).
    #[test]
    fn extra_zip64_primer_record_gana() {
        let mut extra = Vec::new();
        for v in [77u64, 99u64] {
            extra.extend_from_slice(&0x0001u16.to_le_bytes());
            extra.extend_from_slice(&8u16.to_le_bytes());
            extra.extend_from_slice(&v.to_le_bytes());
        }
        let got = resolve_extra(&extra, 5, u32::MAX, 7).expect("válido");
        assert_eq!(got, (5, 77, 7), "el primer record fija uncomp");
    }

    #[test]
    fn extra_que_desborda_el_blob_es_conservador() {
        // size promete 200 con 3 bytes: se deja de caminar, valores del CD.
        let mut extra = 0x9999u16.to_le_bytes().to_vec();
        extra.extend_from_slice(&200u16.to_le_bytes());
        extra.extend_from_slice(&[1, 2, 3]);
        assert_eq!(resolve_extra(&extra, 10, 20, 30), Some((10, 20, 30)));
    }

    /// Par DOS `(y, m, d)` en aritmética (equivale a `y<<9 | m<<5 | d`).
    fn dos_date(y: u16, m: u16, d: u16) -> u16 {
        (y - 1980) * 512 + m * 32 + d
    }

    #[test]
    fn dos_fechas_pineadas() {
        // Epoch DOS: 1980-01-01 00:00:00 → 315532800000 ms.
        assert_eq!(
            dos_pair_to_ms(0, dos_date(1980, 1, 1)),
            Some(315_532_800_000)
        );
        // Bisiesto: 2024-02-29 12:30:10 → 1709209810000 ms.
        let time = 12 * 2048 + 30 * 32 + 5; // h<<11 | min<<5 | segundos/2
        assert_eq!(
            dos_pair_to_ms(time, dos_date(2024, 2, 29)),
            Some(1_709_209_810_000)
        );
        // Mes 0/13 y día 0: defensivo, None.
        assert_eq!(dos_pair_to_ms(0, dos_date(1981, 0, 5)), None); // mes 0
        assert_eq!(dos_pair_to_ms(0, dos_date(1981, 13, 1)), None); // mes 13
        assert_eq!(dos_pair_to_ms(0, dos_date(1981, 1, 0)), None); // día 0
    }

    #[test]
    fn data_offset_valida_firma_y_contenedor() {
        let bytes = ZipSmith::new().file(b"a.txt", b"hola").build();
        let len = bytes.len() as u64;
        let mut reader = Cursor::new(bytes.clone());
        // Entrada única en offset 0: datos tras 30 + name_len.
        let data = data_offset(&mut reader, 0, len).expect("offset");
        assert_eq!(data, 30 + 5);
        assert_eq!(
            &bytes[usize::try_from(data).expect("small")..][..4],
            b"hola"
        );
        // Offset que no apunta a un local header: Corrupt.
        assert_eq!(data_offset(&mut reader, 4, len), Err(Error::Corrupt));
        // Local header cuyo fin sale del contenedor: Corrupt.
        let mut fake = b"PK\x03\x04".to_vec();
        fake.extend_from_slice(&[0u8; 22]);
        fake.extend_from_slice(&u16::MAX.to_le_bytes()); // name_len enorme
        fake.extend_from_slice(&u16::MAX.to_le_bytes()); // extra_len enorme
        let flen = fake.len() as u64;
        let mut fr = Cursor::new(fake);
        assert_eq!(data_offset(&mut fr, 0, flen), Err(Error::Corrupt));
    }

    /// #100.2: una entrada del CD declara un `comment_len` que su `cd_size`
    /// no cubre — el walk se queda corto a mitad del comentario por-entrada.
    /// Pin de `skipped != comment_len → Corrupt` (ZipSmith emitía siempre
    /// `comment_len == 0`, y el mutante que borra el chequeo sobrevivía).
    #[test]
    fn cd_comment_truncado_es_corrupt() {
        let bytes = ZipSmith::new()
            .file(b"real.txt", b"ok")
            .cd_comment_len_lie(10)
            .build();
        let mut reader = Cursor::new(bytes.clone());
        let eocd = locate_eocd(&mut reader, bytes.len() as u64).expect("eocd válido");
        let got = parse_cd(&mut reader, &eocd, &no_cancel(), |_| Ok(()));
        assert_eq!(got.map(|_| ()).unwrap_err(), Error::Corrupt);
    }

    #[test]
    fn cancelacion_corta_el_walk() {
        let bytes = ZipSmith::new().file(b"a", b"x").file(b"b", b"y").build();
        let len = bytes.len() as u64;
        let mut reader = Cursor::new(bytes);
        let eocd = locate_eocd(&mut reader, len).expect("eocd");
        let cancel = Arc::new(AtomicBool::new(true)); // armado ANTES
        let got = parse_cd(&mut reader, &eocd, &cancel, |_| Ok(()));
        assert_eq!(got.map(|_| ()).unwrap_err(), Error::Cancelled);
    }
}
