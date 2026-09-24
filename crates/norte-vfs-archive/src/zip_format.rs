//! Indexing and reading of zip over our OWN central directory parser
//! ([`zip_cd`](crate::zip_cd), #59). SYNC: runs in `spawn_blocking` over a
//! [`ProviderReader`](crate::blocking::ProviderReader).
//!
//! Names: ALWAYS raw bytes from the CD (rule 1). Bit 11 (UTF-8) isn't used
//! to decode anything and the 0x7075 extra is ignored by design; manual
//! display reinterpretation is a future feature (phase 8g issue). An
//! entry's locator is SELF-CONTAINED (`Locator::Zip`): reading resolves
//! the data offset from the LOCAL header and decompresses with flate2 —
//! no retained archive object nor CD re-parse.

use std::io::{Read, Seek, SeekFrom};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use norte_proto::{EntryKind, Error};

use crate::index::{ArchiveIndex, Limits, Locator, Node};
use crate::zip_cd;

/// Builds the index by streaming the central directory (never
/// materialized, #59). `cancel` is checked per entry (rule 3). The
/// EOCD/EOCD64 count cuts short BEFORE paying for the CD if it exceeds
/// `max_entries` (#95.3: `LimitExceeded`, not `Corrupt` — it could be a
/// lying EOCD OR a legitimately huge zip; it refuses to find out). An
/// EOCD that lies LOW also cuts short: the REAL entries count during the walk.
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
            "EOCD exceeds max_entries"
        );
        return Err(Error::LimitExceeded {
            limit: Error::LIMIT_ENTRIES.into(),
        });
    }
    let mut index = ArchiveIndex::new(generation);
    let stats = zip_cd::parse_cd(&mut reader, &eocd, cancel, |entry| {
        // Kind from the RAW bytes, never from decoded metadata: only a
        // trailing `/` is a dir (a trailing `\` is a legal file — H2).
        let node = if entry.name_raw.last() == Some(&b'/') {
            Node::dir(entry.mtime_ms)
        } else {
            let readable = entry.flags & 1 == 0 && (entry.method == 0 || entry.method == 8);
            Node {
                kind: EntryKind::File,
                size: Some(entry.uncomp_size),
                mtime_ms: entry.mtime_ms,
                // No locator: it gets LISTED (metadata) but read →
                // Unsupported (encrypted or a method outside
                // stored/deflate, ADR 0018).
                locator: readable.then_some(Locator::Zip {
                    header_offset: entry.header_offset,
                    method: entry.method,
                    crc32: entry.crc32,
                    comp_size: entry.comp_size,
                    uncomp_size: entry.uncomp_size,
                }),
                link_target: None,
                // ALWAYS, even without a locator (#108 block 2): method is
                // precisely more interesting on encrypted/unsupported entries.
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
                "zip exceeds the omitted-entries budget"
            );
            // #95.3: same criterion as tar/targz — the omitted-entries
            // budget is a LOCAL limit, not corruption.
            return Err(Error::LimitExceeded {
                limit: Error::LIMIT_ENTRIES.into(),
            });
        }
        Ok(())
    })?;
    index.skipped += stats.hostile_skipped;
    if eocd.count != stats.parsed {
        // An EOCD that lies HIGH is stale metadata, not fatal: the
        // entries it announces that don't exist count as omitted (a
        // signal). Lying low was already covered by the budget during the
        // walk. Note #59: the `zip` crate's lossy collapse (H1) can no
        // longer happen — this divergence can only come from the EOCD itself.
        tracing::warn!(
            claimed = eocd.count,
            parsed = stats.parsed,
            "the EOCD's count doesn't match the central directory"
        );
        index.skipped += eocd.count.saturating_sub(stats.parsed);
    }
    // rust MINOR-1 (#59 review): the parser's hostile ones (malformed
    // zip64) and the EOCD's delta come in AFTER the walk — the
    // omitted-entries budget is re-applied here, same criterion as inside
    // the walk.
    if index.skipped > limits.max_entries as u64 {
        tracing::warn!(
            max = limits.max_entries,
            "zip exceeds the omitted-entries budget (post-walk)"
        );
        return Err(Error::LimitExceeded {
            limit: Error::LIMIT_ENTRIES.into(),
        });
    }
    if index.skipped > 0 {
        tracing::warn!(
            skipped = index.skipped,
            "entries omitted from the index (hostile names/limits); detail in debug"
        );
    }
    Ok(index)
}

/// Parameters of a zip read (#59): the self-contained locator plus the
/// range trim the caller ALREADY applied over decompressed bytes.
pub(crate) struct ReadPlan {
    /// The LOCAL header's offset in the container.
    pub header_offset: u64,
    /// Compression method (0 stored / 8 deflate — the locator only exists
    /// for those two).
    pub method: u16,
    /// CRC-32 the CD declares (verified ONLY on complete reads).
    pub crc32: u32,
    /// Compressed size (bounds the decoder's `Take`).
    pub comp_size: u64,
    /// Decompressed size the CD promises.
    pub uncomp_size: u64,
    /// Container size (same generation as the index).
    pub container_len: u64,
    /// Decompressed bytes to skip (the caller's range).
    pub skip: u64,
    /// Decompressed bytes to deliver (the caller's range, already trimmed
    /// against the entry's size).
    pub take: u64,
}

/// Reads an entry into `tx`, resolving the data offset from the LOCAL
/// header — no retained archive nor CD re-parse (#59). stored goes with a
/// direct seek; deflate decompresses in streaming (the range is applied
/// over the DECOMPRESSED bytes via skip/take). On COMPLETE reads (the copy
/// path) the CD's CRC is verified against the bytes served: a mismatch
/// closes the stream with `Err(Corrupt)` as the last item. A partial range
/// is NOT verified (documented: it would require decompressing the whole
/// entry). If the receiver dies (dropping the stream = cancellation, rule
/// 3), `blocking_send` fails and the thread ends at the next chunk.
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
    // CRC only on complete reads: skip==0 and take==the entry's size.
    let crc = (plan.skip == 0 && plan.take == plan.uncomp_size).then(flate2::Crc::new);
    match plan.method {
        0 => {
            // stored: APPNOTE requires comp == uncomp. A CD that lies
            // (`uncomp > comp`) would make a RANGED read silently serve
            // the container's neighboring bytes (encoding review #59's
            // MAJOR-1 — the old crate bounded by comp_size): a fail-loud
            // rejection, never foreign data attributed to the entry.
            if plan.comp_size != plan.uncomp_size {
                tracing::warn!(
                    comp = plan.comp_size,
                    uncomp = plan.uncomp_size,
                    "stored entry with inconsistent sizes in the CD"
                );
                let _ = tx.blocking_send(Err(Error::Corrupt));
                return;
            }
            // Bytes as they are in the container — a direct seek to the
            // requested span, no discard phase.
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
            // The Take bounds the decoder to THIS entry's compressed
            // span: a lying deflate can't drag along the next entry's bytes.
            let mut decoder = flate2::read::DeflateDecoder::new(reader.take(plan.comp_size));
            pump(&mut decoder, plan.skip, plan.take, crc, plan.crc32, tx);
        }
        other => {
            // Unreachable with the index's locators (readable ⇒ 0|8):
            // defensive, never a panic.
            tracing::warn!(method = other, "unsupported zip method in read");
            let _ = tx.blocking_send(Err(Error::Unsupported));
        }
    }
}

/// Pumps `take` bytes (after discarding `to_skip`) from `src` into the
/// channel in 64 KiB chunks. `Ok(0)` in EITHER phase → `Corrupt` (#95.4:
/// the caller already trimmed the range against the entry's size — an EOF
/// here can only be a truncated/mutated container under our feet, never
/// silently short data). With `crc` active (a complete read) the final
/// mismatch is sent as the LAST `Err(Corrupt)` item.
fn pump<R: Read>(
    src: &mut R,
    to_skip: u64,
    take: u64,
    mut crc: Option<flate2::Crc>,
    expected_crc: u32,
    tx: &tokio::sync::mpsc::Sender<Result<bytes::Bytes, Error>>,
) {
    let send_err = |tx: &tokio::sync::mpsc::Sender<Result<bytes::Bytes, Error>>, e: Error| {
        // Best effort: if the receiver died, there's nobody to tell.
        let _ = tx.blocking_send(Err(e));
    };
    let mut buf = vec![0u8; 64 * 1024];
    let mut to_skip = to_skip;
    while to_skip > 0 {
        // The DISCARD phase sends nothing to the channel: without this
        // check, a caller that drops the stream mid a deep skip (ranged
        // deflate over a zip64 entry) would leave the blocking thread
        // pinned decompressing for nobody (rust MAJOR-1 from review #59;
        // rule 3).
        if tx.is_closed() {
            tracing::debug!("zip read cancelled during discard (receiver dead)");
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
            // Premature EOF mid-entry: the index promised `size` bytes
            // and they aren't there — short data is NEVER silent.
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
                    tracing::debug!("zip read cancelled (receiver dead)");
                    return;
                }
            }
            Err(e) => return send_err(tx, zip_cd::corrupt_io(&e)),
        }
    }
    if let Some(crc) = crc
        && crc.sum() != expected_crc
    {
        tracing::warn!("the CD's CRC doesn't match the bytes served");
        send_err(tx, Error::Corrupt);
    }
}
