//! OUR OWN zip central directory parser (#59): EOCD/EOCD64 + streaming CD
//! walk + resolving the data offset from the LOCAL header. SYNC: runs in
//! `spawn_blocking` over a [`ProviderReader`](crate::blocking::ProviderReader).
//!
//! Replaces the `zip` crate on the indexing/reading path:
//! - Names are the CD's RAW bytes, verbatim (rule 1) — no lossy collapse
//!   of names that decode the same (H1).
//! - The 0x7075 extra (Info-ZIP unicode path) is IGNORED BY DESIGN: it
//!   never substitutes the name nor kills the archive (H3).
//! - The CD is NEVER materialized whole: it's walked entry by entry under
//!   a `Take` (`Limits::max_cd_bytes` is obsolete — there's no retained
//!   memory left to govern).
//! - zip64: the EOCD's markers lead to the EOCD64 via its locator; the
//!   real 64-bit count enters `max_entries`'s preflight (the u16
//!   preflight gap is closed).

use std::io::{Read, Seek, SeekFrom};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use norte_proto::Error;

const EOCD_SIG: [u8; 4] = [0x50, 0x4b, 0x05, 0x06];
const EOCD64_SIG: [u8; 4] = [0x50, 0x4b, 0x06, 0x06];
const EOCD64_LOCATOR_SIG: [u8; 4] = [0x50, 0x4b, 0x06, 0x07];
const CD_ENTRY_SIG: [u8; 4] = [0x50, 0x4b, 0x01, 0x02];
const LOCAL_SIG: [u8; 4] = [0x50, 0x4b, 0x03, 0x04];

/// Already-resolved end of central directory (classic or zip64): what
/// indexing needs for the entry preflight and the CD walk.
pub(crate) struct Eocd {
    /// Entries the EOCD declares (can lie in either direction).
    pub count: u64,
    /// The central directory's offset in the container.
    pub cd_offset: u64,
    /// Bytes of the central directory.
    pub cd_size: u64,
}

/// A central directory entry, with its zip64 fields ALREADY resolved.
pub(crate) struct CdEntry {
    /// Name in RAW bytes from the CD, verbatim (rule 1).
    pub name_raw: Vec<u8>,
    /// General purpose flags (bit 0 = encrypted).
    pub flags: u16,
    /// Compression method (0 stored / 8 deflate / others).
    pub method: u16,
    /// Declared CRC-32 of the UNcompressed bytes.
    pub crc32: u32,
    /// Compressed size.
    pub comp_size: u64,
    /// Uncompressed size.
    pub uncomp_size: u64,
    /// The LOCAL header's offset in the container.
    pub header_offset: u64,
    /// mtime in ms since epoch (DOS time interpreted as UTC), if the DOS
    /// pair is valid.
    pub mtime_ms: Option<i64>,
}

/// Result of the CD walk: how many entries were consumed and how many of
/// them were hostile (omitted WITHOUT being handed to `per_entry`).
/// `parsed` INCLUDES the hostile ones: the comparison against
/// `Eocd::count` is over what was really consumed from the CD.
pub(crate) struct ParseStats {
    /// Entries consumed from the CD (delivered + hostile).
    pub parsed: u64,
    /// Subset omitted for a malformed zip64 extra (a marker with no value).
    pub hostile_skipped: u64,
}

/// u16 LE at `off`. Caller invariant: `off + 2 <= b.len()` (constant
/// offsets over fixed-size or pre-checked buffers).
fn le16(b: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([b[off], b[off + 1]])
}

/// u32 LE at `off`. Caller invariant: `off + 4 <= b.len()`.
fn le32(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(
        b[off..off + 4]
            .try_into()
            .expect("constant range inside the buffer"),
    )
}

/// u64 LE at `off`. Caller invariant: `off + 8 <= b.len()`.
fn le64(b: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(
        b[off..off + 8]
            .try_into()
            .expect("constant range inside the buffer"),
    )
}

/// Locates the EOCD by scanning the last window backward (22 bytes + a
/// 65535 max comment). A comment CAN CONTAIN the signature (H9): a
/// candidate is only valid if it's self-consistent — `cd_offset +
/// cd_size` points exactly at its position — or if its zip64 branch
/// (markers → locator → EOCD64) is consistent. No locatable EOCD →
/// `Corrupt`.
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
            // zip64 markers: the real count/size/offset are in the
            // EOCD64, located by the 20-byte record JUST BEFORE the EOCD.
            if let Some(eocd) = read_zip64(reader, candidate_pos)? {
                return Ok(eocd);
            }
            continue; // marker with no consistent zip64 chain: keep going backward
        }
        if u64::from(cd_off) + u64::from(cd_size) == candidate_pos {
            return Ok(Eocd {
                count: u64::from(count),
                cd_offset: u64::from(cd_off),
                cd_size: u64::from(cd_size),
            });
        }
    }
    tracing::warn!("no locatable EOCD: not a zip");
    Err(Error::Corrupt)
}

/// A `read_exact` that distinguishes a structural EOF (`Ok(false)`: the
/// candidate isn't zip64, the caller keeps searching) from genuine IO from
/// the inner provider (verbatim, #58).
fn read_exact_or_eof<R: Read>(reader: &mut R, buf: &mut [u8]) -> Result<bool, Error> {
    match reader.read_exact(buf) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => Ok(false),
        Err(e) => Err(corrupt_io(&e)),
    }
}

/// The zip64 branch of an EOCD candidate with markers: a 20-byte locator
/// right before the EOCD → the EOCD64's offset → 64-bit
/// `count`/`cd_size`/`cd_offset`. `Ok(None)` = inconsistent structure (the
/// caller keeps searching backward); `Err` only for genuine IO from the
/// inner provider.
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
        return Ok(None); // the EOCD64 must live BEFORE its locator
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
    // Consistency (parity with the classic rule): the CD ends at most
    // where the EOCD64 starts.
    match cd_offset.checked_add(cd_size) {
        Some(end) if end <= eocd64_pos => Ok(Some(Eocd {
            count,
            cd_offset,
            cd_size,
        })),
        _ => Ok(None),
    }
}

/// Resolves the final comp/uncomp/`header_offset` by walking the CD's
/// extra blob. `None` = a HOSTILE entry (a zip64 marker with no 64-bit
/// value): the ENTRY is omitted, it never kills the archive. Truncated
/// records or ones that overflow the blob: stops walking and the CD's
/// values stand (conservative — the entry survives). Id 0x7075 (Info-ZIP
/// unicode path) is IGNORED BY DESIGN: it never substitutes `name_raw` (H3).
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
            break; // truncated record: the CD's values stand, the entry survives
        }
        if id == 0x0001 && !resolved {
            // zip64: u64 values in APPNOTE order — uncomp, comp, header
            // offset (the disk number u32 goes last, ignored) — ONLY for
            // the fields whose CD value is the 0xFFFF_FFFF marker.
            // Documented interop (Go's archive/zip style): a non-conforming
            // writer emitting the full triplet while marking only some
            // fields gets misread — strict on purpose.
            let mut body = &extra[pos + 4..end];
            for (needed, slot) in [
                (uncomp32 == u32::MAX, &mut uncomp),
                (comp32 == u32::MAX, &mut comp),
                (off32 == u32::MAX, &mut off),
            ] {
                if needed {
                    if body.len() < 8 {
                        return None; // a marker with no value: hostile
                    }
                    *slot = le64(body, 0);
                    body = &body[8..];
                }
            }
            // The FIRST 0x0001 record wins (review #59): a second one
            // can't rewrite the values (hostile ambiguity).
            resolved = true;
        }
        // 0x7075 and other ids: ignored (see the module doc).
        pos = end;
    }
    if any_marker && !resolved {
        // Fields marked 0xFFFF_FFFF with NO 0x0001 record at all: the CD
        // promises zip64 and doesn't deliver — hostile (previously the
        // literal 0xFFFFFFFF snuck in as a lying 4 GiB−1 size/offset).
        return None;
    }
    Some((comp, uncomp, off))
}

/// Walks the central directory in STREAMING fashion (never materialized:
/// a `Take` of `cd_size`) delivering each entry to `per_entry`;
/// `per_entry`'s error cuts it short and is propagated. `cancel` is
/// checked per entry (rule 3). Broken structure (invalid signature, CD
/// truncated mid-entry) → `Corrupt`; a hostile entry (zip64 extra with a
/// marker and no value) is OMITTED with `warn!` and counts in
/// [`ParseStats::hostile_skipped`], without killing the archive.
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
            tracing::debug!("central directory parse cancelled");
            return Err(Error::Cancelled);
        }
        let mut header = [0u8; 46];
        cd.read_exact(&mut header).map_err(|e| corrupt_io(&e))?;
        if header[..4] != CD_ENTRY_SIG {
            tracing::warn!("invalid central directory entry signature");
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
        // The comment is skipped without materializing it; coming up
        // short is a truncated CD (#95.4: never silence mid-structure).
        let skipped = std::io::copy(&mut (&mut cd).take(comment_len), &mut std::io::sink())
            .map_err(|e| corrupt_io(&e))?;
        if skipped != comment_len {
            tracing::warn!("central directory truncated mid-comment");
            return Err(Error::Corrupt);
        }
        stats.parsed += 1;
        let Some((comp_size, uncomp_size, header_offset)) =
            resolve_extra(&extra, comp32, uncomp32, off32)
        else {
            stats.hostile_skipped += 1;
            // **`debug!` and no name, and that's a size decision.** This
            // `warn!` used to carry the entry's RAW name — up to 64 KiB,
            // chosen by whoever made the zip — and a minimal hostile
            // entry costs 46 bytes of container. While the log was an
            // ephemeral terminal that was noise; since there's a file
            // (roadmap item 9) it's amplification: a 50 MB zip dropped in
            // a directory someone browses — listing an archive asks for
            // no confirmation — writes hundreds of MB to that day's log,
            // and escaping the control bytes multiplies it by six. The
            // aggregate total IS reported (`hostile_skipped`), which is
            // the operational signal; the per-entry detail lives in
            // `debug`, like the rest of this parser already says.
            tracing::debug!("zip64 extra with a marker and no value: entry omitted (hostile)");
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

/// Offset of an entry's FIRST data byte: skips the LOCAL header (30 fixed
/// bytes + LOCAL name/extra — may differ from the CD's copies). `Corrupt`
/// if the signature doesn't match or the resulting offset falls outside
/// the container.
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
        tracing::warn!("invalid local header signature");
        return Err(Error::Corrupt);
    }
    let name_len = u64::from(le16(&local, 26));
    let extra_len = u64::from(le16(&local, 28));
    let data = header_offset
        .checked_add(30 + name_len + extra_len)
        .ok_or(Error::Corrupt)?;
    if data > container_len {
        tracing::warn!("local header points outside the container");
        return Err(Error::Corrupt);
    }
    Ok(data)
}

/// Civil epoch → days since 1970-01-01 (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Raw DOS `(time, date)` pair → ms since epoch. DOS time carries no zone:
/// interpreted as UTC (a documented approximation; phase 8g debt issue).
/// Month 0/>12 or day 0 → `None` (defensive: the pair comes from hostile
/// bytes, not a clock).
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

/// Genuine IO from the inner provider (a network drop mid-parse): it's
/// propagated VERBATIM, never disguised as `Corrupt` (#58). Everything
/// else (a premature EOF, garbage) IS broken structure.
pub(crate) fn corrupt_io(e: &std::io::Error) -> Error {
    if let Some(inner) = crate::blocking::inner_proto_error(e) {
        return inner;
    }
    tracing::warn!(error = %e, "corrupt or unreadable zip");
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
    fn eocd_with_a_fake_signature_in_the_comment() {
        // H9: the comment's candidate isn't self-consistent — the search
        // keeps going backward to the real EOCD.
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
    fn zip64_roundtrip_of_the_tail() {
        let bytes = ZipSmith::new().file(b"z.txt", b"abc").build_zip64();
        let (eocd, entries, stats) = collect_cd(&bytes);
        assert_eq!(eocd.count, 1);
        assert_eq!(stats.parsed, 1);
        assert_eq!(entries[0].name_raw, b"z.txt");
        assert_eq!(entries[0].uncomp_size, 3);
    }

    /// A 56-byte EOCD64 with `count`/`cd_size`/`cd_offset` at the
    /// positions [`read_zip64`] reads. The rest is zeroed (enough for the pin).
    fn eocd64_record(count: u64, cd_size: u64, cd_offset: u64) -> [u8; 56] {
        let mut r = [0u8; 56];
        r[..4].copy_from_slice(&EOCD64_SIG);
        r[4..12].copy_from_slice(&44u64.to_le_bytes()); // size of the rest of the record
        r[32..40].copy_from_slice(&count.to_le_bytes());
        r[40..48].copy_from_slice(&cd_size.to_le_bytes());
        r[48..56].copy_from_slice(&cd_offset.to_le_bytes());
        r
    }

    /// A 20-byte locator pointing at `eocd64_pos`.
    fn eocd64_locator(eocd64_pos: u64) -> [u8; 20] {
        let mut l = [0u8; 20];
        l[..4].copy_from_slice(&EOCD64_LOCATOR_SIG);
        l[8..16].copy_from_slice(&eocd64_pos.to_le_bytes());
        l[16..20].copy_from_slice(&1u32.to_le_bytes()); // total disks
        l
    }

    /// #100.1 (surviving mutant from audit #59): [`read_zip64`]'s
    /// `eocd64_pos >= locator_pos` guard. A forged locator points
    /// FORWARD, at an otherwise consistent EOCD64; the EOCD64 must live
    /// BEFORE its locator, so the chain gets discarded (`None`) and the
    /// scan keeps going toward the real EOCD. Without the guard, the
    /// mutant would accept this forged EOCD64.
    #[test]
    fn read_zip64_rejects_an_eocd64_not_before_its_locator() {
        let mut buf = vec![0u8; 128];
        buf[0..20].copy_from_slice(&eocd64_locator(20)); // locator_pos = 0
        buf[20..76].copy_from_slice(&eocd64_record(1, 0, 0)); // end 0 <= 20
        let mut r = Cursor::new(buf);
        // eocd_pos = locator_pos + 20; eocd64_pos (20) >= locator_pos (0).
        assert!(read_zip64(&mut r, 20).expect("io").is_none());
    }

    /// #100.1 (second mutant from audit #59): [`read_zip64`]'s `end <=
    /// eocd64_pos` guard. A consistent locator→EOCD64 chain except the CD
    /// promises to end BEYOND the EOCD64 (`cd_offset + cd_size >
    /// eocd64_pos`): inconsistent, discarded. Without the guard, the
    /// mutant would accept a lying `cd_offset`/`cd_size`.
    #[test]
    fn read_zip64_rejects_a_cd_that_overflows_the_eocd64() {
        let mut buf = vec![0u8; 128];
        buf[0..56].copy_from_slice(&eocd64_record(1, 100, 0)); // end 100 > 0
        buf[56..76].copy_from_slice(&eocd64_locator(0)); // locator_pos = 56
        let mut r = Cursor::new(buf);
        // eocd_pos = 76; eocd64_pos (0) < locator_pos (56) passes guard 174.
        assert!(read_zip64(&mut r, 76).expect("io").is_none());
    }

    #[test]
    fn extra_zip64_resolves_the_markers_in_order() {
        // uncomp and offset marked; comp normal: the blob carries TWO
        // u64s (uncomp, offset) — APPNOTE order skipping comp.
        let mut extra = 0x0001u16.to_le_bytes().to_vec();
        extra.extend_from_slice(&16u16.to_le_bytes());
        extra.extend_from_slice(&77u64.to_le_bytes()); // uncomp
        extra.extend_from_slice(&99u64.to_le_bytes()); // header offset
        let got = resolve_extra(&extra, 5, u32::MAX, u32::MAX).expect("valid");
        assert_eq!(got, (5, 77, 99));
    }

    #[test]
    fn extra_zip64_marker_without_a_value_is_hostile() {
        // uncomp marked but the record only carries 4 bytes: hostile (None).
        let mut extra = 0x0001u16.to_le_bytes().to_vec();
        extra.extend_from_slice(&4u16.to_le_bytes());
        extra.extend_from_slice(&[0u8; 4]);
        assert!(resolve_extra(&extra, 0, u32::MAX, 0).is_none());
    }

    /// enc MAJOR-2 (review #59): the CANONICAL case — all THREE fields
    /// marked, three distinct values — pins down the full APPNOTE order
    /// (uncomp, comp, offset). A comp/uncomp permutation here is exactly
    /// the mutant that survived the suite.
    #[test]
    fn extra_zip64_three_markers_canonical_order() {
        let mut extra = 0x0001u16.to_le_bytes().to_vec();
        extra.extend_from_slice(&24u16.to_le_bytes());
        extra.extend_from_slice(&111u64.to_le_bytes()); // uncomp
        extra.extend_from_slice(&222u64.to_le_bytes()); // comp
        extra.extend_from_slice(&333u64.to_le_bytes()); // header offset
        let got = resolve_extra(&extra, u32::MAX, u32::MAX, u32::MAX).expect("valid");
        assert_eq!(got, (222, 111, 333), "exact (comp, uncomp, off)");
    }

    /// Review #59: fields MARKED with no 0x0001 record at all in the
    /// extra — the CD promises zip64 and doesn't deliver: hostile
    /// (previously the literal 0xFFFFFFFF snuck in as a lying 4 GiB−1 size).
    #[test]
    fn extra_zip64_marker_without_a_record_is_hostile() {
        // Extra with only a 0x7075 (ignored): the marker is left with no value.
        let mut extra = 0x7075u16.to_le_bytes().to_vec();
        extra.extend_from_slice(&1u16.to_le_bytes());
        extra.push(1);
        assert!(resolve_extra(&extra, u32::MAX, 0, 0).is_none());
        // EMPTY extra with a marker: same thing.
        assert!(resolve_extra(&[], 0, u32::MAX, 0).is_none());
    }

    /// Review #59: the FIRST 0x0001 record wins — a second record doesn't
    /// rewrite the values (hostile ambiguity resolved conservatively).
    #[test]
    fn extra_zip64_first_record_wins() {
        let mut extra = Vec::new();
        for v in [77u64, 99u64] {
            extra.extend_from_slice(&0x0001u16.to_le_bytes());
            extra.extend_from_slice(&8u16.to_le_bytes());
            extra.extend_from_slice(&v.to_le_bytes());
        }
        let got = resolve_extra(&extra, 5, u32::MAX, 7).expect("valid");
        assert_eq!(got, (5, 77, 7), "the first record fixes uncomp");
    }

    #[test]
    fn extra_that_overflows_the_blob_is_conservative() {
        // size promises 200 with 3 bytes: stops walking, uses the CD's values.
        let mut extra = 0x9999u16.to_le_bytes().to_vec();
        extra.extend_from_slice(&200u16.to_le_bytes());
        extra.extend_from_slice(&[1, 2, 3]);
        assert_eq!(resolve_extra(&extra, 10, 20, 30), Some((10, 20, 30)));
    }

    /// DOS `(y, m, d)` pair in arithmetic (equivalent to `y<<9 | m<<5 | d`).
    fn dos_date(y: u16, m: u16, d: u16) -> u16 {
        (y - 1980) * 512 + m * 32 + d
    }

    #[test]
    fn dos_dates_pinned() {
        // DOS epoch: 1980-01-01 00:00:00 → 315532800000 ms.
        assert_eq!(
            dos_pair_to_ms(0, dos_date(1980, 1, 1)),
            Some(315_532_800_000)
        );
        // Leap year: 2024-02-29 12:30:10 → 1709209810000 ms.
        let time = 12 * 2048 + 30 * 32 + 5; // h<<11 | min<<5 | seconds/2
        assert_eq!(
            dos_pair_to_ms(time, dos_date(2024, 2, 29)),
            Some(1_709_209_810_000)
        );
        // Month 0/13 and day 0: defensive, None.
        assert_eq!(dos_pair_to_ms(0, dos_date(1981, 0, 5)), None); // month 0
        assert_eq!(dos_pair_to_ms(0, dos_date(1981, 13, 1)), None); // month 13
        assert_eq!(dos_pair_to_ms(0, dos_date(1981, 1, 0)), None); // day 0
    }

    #[test]
    fn data_offset_validates_signature_and_container() {
        let bytes = ZipSmith::new().file(b"a.txt", b"data").build();
        let len = bytes.len() as u64;
        let mut reader = Cursor::new(bytes.clone());
        // A single entry at offset 0: data after 30 + name_len.
        let data = data_offset(&mut reader, 0, len).expect("offset");
        assert_eq!(data, 30 + 5);
        assert_eq!(
            &bytes[usize::try_from(data).expect("small")..][..4],
            b"data"
        );
        // An offset that doesn't point at a local header: Corrupt.
        assert_eq!(data_offset(&mut reader, 4, len), Err(Error::Corrupt));
        // A local header whose end falls outside the container: Corrupt.
        let mut fake = b"PK\x03\x04".to_vec();
        fake.extend_from_slice(&[0u8; 22]);
        fake.extend_from_slice(&u16::MAX.to_le_bytes()); // huge name_len
        fake.extend_from_slice(&u16::MAX.to_le_bytes()); // huge extra_len
        let flen = fake.len() as u64;
        let mut fr = Cursor::new(fake);
        assert_eq!(data_offset(&mut fr, 0, flen), Err(Error::Corrupt));
    }

    /// #100.2: a CD entry declares a `comment_len` its `cd_size` doesn't
    /// cover — the walk comes up short mid the per-entry comment. Pins
    /// `skipped != comment_len → Corrupt` (`ZipSmith` always emitted
    /// `comment_len == 0`, and the mutant that removes the check survived).
    #[test]
    fn cd_comment_truncated_is_corrupt() {
        let bytes = ZipSmith::new()
            .file(b"real.txt", b"ok")
            .cd_comment_len_lie(10)
            .build();
        let mut reader = Cursor::new(bytes.clone());
        let eocd = locate_eocd(&mut reader, bytes.len() as u64).expect("valid eocd");
        let got = parse_cd(&mut reader, &eocd, &no_cancel(), |_| Ok(()));
        assert_eq!(got.map(|_| ()).unwrap_err(), Error::Corrupt);
    }

    #[test]
    fn cancellation_cuts_the_walk_short() {
        let bytes = ZipSmith::new().file(b"a", b"x").file(b"b", b"y").build();
        let len = bytes.len() as u64;
        let mut reader = Cursor::new(bytes);
        let eocd = locate_eocd(&mut reader, len).expect("eocd");
        let cancel = Arc::new(AtomicBool::new(true)); // armed BEFORE
        let got = parse_cd(&mut reader, &eocd, &cancel, |_| Ok(()));
        assert_eq!(got.map(|_| ()).unwrap_err(), Error::Cancelled);
    }
}
