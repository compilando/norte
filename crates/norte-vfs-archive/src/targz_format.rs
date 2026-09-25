//! Indexing and reading of tar.gz/tgz (ADR 0028, #55): composite `tar+gz`
//! — the gz layer is OPAQUE over tar, forward-only (not seekable). The
//! index walks `tar::Archive::entries()` (`Read` only, no
//! `entries_with_seek`) over a `flate2::read::MultiGzDecoder` (concatenated
//! gzip members: real tgz files have them); `Locator::Gz`'s offsets are of
//! the DECOMPRESSED stream. SYNC: runs in `spawn_blocking` over a
//! [`ProviderReader`](crate::blocking::ProviderReader), same as
//! [`tar_format`](crate::tar_format)/[`zip_format`](crate::zip_format) —
//! in fact it reuses `tar_format`'s entry classification
//! (`classify_entry`/`EntryShape`): name/kind/symlink/mtime are IDENTICAL
//! to plain tar, only how a regular file's `Locator` is resolved changes.

use std::io::Read;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use flate2::read::MultiGzDecoder;
use norte_proto::{EntryKind, Error};

use crate::index::{ArchiveIndex, Limits, Locator, Node};
use crate::tar_format::{EntryShape, classify_entry};

/// Wraps the gzip DECODER (not the container's raw `Read`) to count
/// DECOMPRESSED bytes read: a gzip bomb is infinite CPU even if the
/// pipeline's memory is streaming (the decoder never materializes the
/// full content) — `max` cuts the WHOLE INDEX PASS short (ADR 0028 D4),
/// not per entry.
///
/// Also arms cancellation (rule 3) PER CHUNK, not just per entry: the
/// internal `tar::Entries` can consume an entry's whole body (or discard
/// its leftover bytes when skipping to the next one) WITHOUT returning
/// control to `build_index_gz`'s outer loop — checking `cancel` only
/// between entries wouldn't be enough to quickly cut short a giant entry.
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
                "tar.gz exceeds the index's decompression budget (bomb)"
            );
            // #95.3: bomb OR a legitimately huge backup — an honest local limit.
            // `inner_proto_error` unwraps it from the io::Error chain.
            return Err(std::io::Error::other(Error::LimitExceeded {
                limit: Error::LIMIT_DECOMPRESSED_BYTES.into(),
            }));
        }
        Ok(n)
    }
}

/// Builds a tar.gz's index by walking `entries()` (forward-only, no
/// `Seek`) over the `MultiGzDecoder`. Reuses
/// [`tar_format`](crate::tar_format)'s `classify_entry` for
/// name/kind/symlink/mtime; only differs in a regular file's `Locator`:
/// DECOMPRESSED offset, WITHOUT validating against any `container_len`
/// (ADR 0028 — that size would be the COMPRESSED one and bounds nothing
/// about the decompressed stream). Truncation is detected fail-loud in
/// `entries()` itself when the decoder cuts off mid-entry (via `corrupt`,
/// same #58 criterion as tar/zip: genuine IO from the inner provider
/// propagates verbatim).
///
/// `cancel` is checked per entry (parity with plain tar) AND per chunk
/// inside `CountingReader` (finer-grained: a single giant entry shouldn't
/// block cancellation).
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
            tracing::debug!("tar.gz indexing cancelled");
            return Err(Error::Cancelled);
        }
        let entry = entry.map_err(|e| corrupt(&e))?;
        let Some((raw_name, mtime_ms, shape)) = classify_entry(&entry) else {
            continue; // meta already consumed by the iterator (pax_global_header)
        };
        let node = match shape {
            EntryShape::Dir => Node::dir(mtime_ms),
            EntryShape::Symlink(link_target) => Node {
                kind: EntryKind::Symlink,
                size: None,
                mtime_ms,
                locator: None,
                link_target,
                zip: None,
            },
            EntryShape::File { offset, size } => Node {
                kind: EntryKind::File,
                size: Some(size),
                mtime_ms,
                locator: Some(Locator::Gz { offset, size }),
                link_target: None,
                zip: None,
            },
            EntryShape::Other => Node {
                kind: EntryKind::Other,
                size: None,
                mtime_ms,
                locator: None,
                link_target: None,
                zip: None,
            },
        };
        index.insert_entry(&raw_name, node, limits)?;
        // Omitted entries also spend budget: a tar.gz with millions of
        // hostile names doesn't iterate for free (same criterion as tar/zip).
        if index.skipped > limits.max_entries as u64 {
            tracing::warn!(
                max = limits.max_entries,
                "tar.gz exceeds the omitted-entries budget"
            );
            return Err(Error::LimitExceeded {
                limit: Error::LIMIT_ENTRIES.into(),
            });
        }
    }
    if index.skipped > 0 {
        tracing::warn!(
            skipped = index.skipped,
            "entries omitted from the index (hostile names/limits); detail in debug"
        );
    }
    Ok(index)
}

/// Reads `take` decompressed bytes starting at `skip` of a tar.gz. A FRESH
/// decoder per read (gz isn't seekable, ADR 0028 D3): discards `skip`
/// bytes in chunks — CHECKING `tx.is_closed()` on every chunk, because a
/// long discard (a large offset inside an entry) must also be cancellable
/// (rule 3; unlike zip's discard, which is short due to deflate's window)
/// — and then serves `take` in 64 KiB chunks over the bounded channel.
///
/// A premature EOF is FAIL-LOUD in BOTH phases, `skip` AND `take` (fix
/// from review #55: the discard phase used to silently return an empty
/// stream — INCORRECT). The caller (`ArchiveProvider::read`) already
/// trimmed `req_off` against `entry_size` BEFORE launching this thread
/// (pread semantics): an EOF here is NEVER "an offset legitimately
/// outside the entry" (the caller already filtered that) — it can only
/// mean a truncated container or one mutated under our feet (the same
/// event [`ProviderReader::read`](crate::blocking::ProviderReader)
/// documents), never silently short data.
pub(crate) fn read_entry_gz<R: Read>(
    reader: R,
    skip: u64,
    take: u64,
    tx: &tokio::sync::mpsc::Sender<Result<bytes::Bytes, Error>>,
) {
    let send_err = |tx: &tokio::sync::mpsc::Sender<Result<bytes::Bytes, Error>>, e: Error| {
        // Best effort: if the receiver died, there's nobody to tell.
        let _ = tx.blocking_send(Err(e));
    };
    let mut decoder = MultiGzDecoder::new(reader);
    let mut buf = vec![0u8; 64 * 1024];
    let mut to_skip = skip;
    while to_skip > 0 {
        if tx.is_closed() {
            tracing::debug!("tar.gz discard cancelled (receiver dead)");
            return;
        }
        let want = buf.len().min(usize::try_from(to_skip).unwrap_or(buf.len()));
        match decoder.read(&mut buf[..want]) {
            Ok(0) => {
                // FIX-1 (rust+security MAJOR, #55 review): the caller
                // ALREADY trimmed `req_off` against `entry_size` — an EOF
                // here can only be a truncated/mutated container under
                // our feet, never a legitimately empty offset. Fail-loud,
                // same as the `take` phase's premature EOF.
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
                // Premature EOF mid-entry: the index promised `size`
                // bytes and the decoder doesn't have them — a container
                // truncated under the entry (never silently short data).
                return send_err(tx, Error::Corrupt);
            }
            Ok(n) => {
                remaining -= n as u64;
                if tx
                    .blocking_send(Ok(bytes::Bytes::copy_from_slice(&buf[..n])))
                    .is_err()
                {
                    tracing::debug!("tar.gz read cancelled (receiver dead)");
                    return;
                }
            }
            Err(e) => return send_err(tx, corrupt(&e)),
        }
    }
}

/// Why [`spool_gz`] aborted without producing a complete spool.
#[derive(Debug)]
pub(crate) enum SpoolAbort {
    /// `tx_probe` returned `true` (receiver dead): nobody's waiting for
    /// the result — discard the partial without noise (rule 3).
    Cancelled,
    /// The decompressed size exceeded the budget
    /// (`Limits::spool_max_bytes`): the container is non-spoolable — the
    /// caller remembers this and reads continue via forward-decode.
    OverBudget,
    /// A genuine error: the decoder (broken gz/IO from the inner
    /// provider, already unwrapped verbatim via [`corrupt`]) or writing
    /// to the spool file (full disk). The caller decides whether to
    /// propagate it or degrade to forward-decode.
    Io(Error),
}

/// Decompresses a container's WHOLE gz stream (from offset 0, the same
/// `MultiGzDecoder` as [`read_entry_gz`]) into the `out` file: #95.1's
/// spool. Returns the total decompressed bytes written.
///
/// `tx_probe` is consulted BEFORE every chunk (`true` = abort): the
/// caller passes it its delivery channel's `tx.is_closed()` — a dead
/// receiver cuts the build short just like it cuts forward-decode short
/// (rule 3). Exceeding `budget` aborts with [`SpoolAbort::OverBudget`]
/// without paying for more decompression (same anti-bomb criterion as the
/// index pass).
pub(crate) fn spool_gz<R: Read>(
    src: R,
    budget: u64,
    tx_probe: &dyn Fn() -> bool,
    out: &mut std::fs::File,
) -> Result<u64, SpoolAbort> {
    use std::io::Write as _;
    let mut decoder = MultiGzDecoder::new(src);
    let mut buf = vec![0u8; 64 * 1024];
    let mut total: u64 = 0;
    loop {
        if tx_probe() {
            return Err(SpoolAbort::Cancelled);
        }
        match decoder.read(&mut buf) {
            Ok(0) => return Ok(total),
            Ok(n) => {
                total += n as u64;
                if total > budget {
                    return Err(SpoolAbort::OverBudget);
                }
                if let Err(e) = out.write_all(&buf[..n]) {
                    // Local spool write (full disk…): doesn't go through
                    // `corrupt` — it isn't the container, it's our tempfile.
                    tracing::warn!(error = %e, "failed writing the tar.gz spool");
                    return Err(SpoolAbort::Io(Error::Io { retryable: true }));
                }
            }
            Err(e) => return Err(SpoolAbort::Io(corrupt(&e))),
        }
    }
}

fn corrupt(e: &std::io::Error) -> Error {
    // Genuine IO from the inner provider (a network drop mid-parse) OR a
    // signal wrapped by `CountingReader` (bomb's `Cancelled`/`Corrupt`):
    // both go through the same `io::Error::other`, unwrapped verbatim
    // (#58) — never disguised as "corrupt tar.gz".
    if let Some(inner) = crate::blocking::inner_proto_error(e) {
        return inner;
    }
    tracing::warn!(error = %e, "corrupt or unreadable tar.gz");
    Error::Corrupt
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write as _};

    fn limits() -> Limits {
        Limits::default()
    }

    /// Gzips already-built bytes (e.g. a `TarSmith` tar) into a single gzip member.
    fn gzip(bytes: &[u8]) -> Vec<u8> {
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(bytes).expect("write gz");
        enc.finish().expect("finish gz")
    }

    /// Verification the plan requires (#55 T3): `entry.raw_file_position()`
    /// WORKS as the DECOMPRESSED offset with `entries()` (no `Seek`) — the
    /// `tar` crate computes it by counting bytes consumed from the `Read`,
    /// not via `Seek::stream_position`. Two files with known content: the
    /// second CAN'T be at offset 0, and the offset must match what
    /// `entries_with_seek` produces over the SAME plain tar.
    #[test]
    fn raw_file_position_is_correct_without_seek() {
        let tar = norte_testkit::TarSmith::new()
            .file(b"first.bin", &[0xAAu8; 600])
            .file(b"second.bin", b"0123456789")
            .build();

        // Reference offsets: with Seek over the PLAIN tar (no gz).
        let mut archive_seek = tar::Archive::new(Cursor::new(tar.clone()));
        let seek_offsets: Vec<(Vec<u8>, u64, u64)> = archive_seek
            .entries_with_seek()
            .expect("entries_with_seek")
            .map(|e| {
                let e = e.expect("entry");
                (e.path_bytes().to_vec(), e.raw_file_position(), e.size())
            })
            .collect();
        assert_eq!(seek_offsets.len(), 2, "two files in the reference tar");
        assert!(
            seek_offsets[1].1 > 0,
            "the second file can't start at offset 0"
        );

        // Same offsets, now via `entries()` (no Seek) over the PLAIN tar
        // directly (no gz in between: isolates raw_file_position's property).
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
            "raw_file_position() matches between entries() and entries_with_seek()"
        );

        // And now via the real pipeline: gz + MultiGzDecoder + entries().
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
            "raw_file_position() over MultiGzDecoder gives the correct DECOMPRESSED offset"
        );
    }

    /// Cancellation cuts the loop short at the next entry (rule 3), same
    /// pattern as plain tar.
    #[test]
    fn cancellation_cuts_indexing_short() {
        let mut smith = norte_testkit::TarSmith::new();
        for i in 0..50u32 {
            smith = smith.file(format!("f{i}").as_bytes(), b"x");
        }
        let gz = gzip(&smith.build());
        let cancel = Arc::new(AtomicBool::new(true)); // armed BEFORE
        let got = build_index_gz(Cursor::new(gz), (Some(0), Some(1)), &limits(), &cancel);
        assert_eq!(got.map(|_| ()).unwrap_err(), Error::Cancelled);
    }

    /// `max_decompressed_bytes` cuts the index pass short without
    /// hanging: a classic "bomb" fixture (a large file of zeros compresses
    /// to almost nothing).
    #[test]
    fn max_decompressed_bytes_cuts_the_bomb_short() {
        let tar = norte_testkit::TarSmith::new()
            .file(b"bomb.bin", &vec![0u8; 2_000_000])
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
                limit: Error::LIMIT_DECOMPRESSED_BYTES.into()
            }
        );
    }

    /// FIX-1 (rust+security MAJOR, #55 review): EOF during DISCARD
    /// (`skip`) must be fail-loud, not a silent empty stream. `skip` here
    /// exceeds what the truncated gz can deliver — before the fix this
    /// returned Ok(()) with no message over the channel at all
    /// (indistinguishable from "no more data because the receiver
    /// closed"); now exactly ONE `Err(Corrupt)` message must arrive.
    #[test]
    fn eof_during_discard_is_corrupt_not_empty() {
        let tar = norte_testkit::TarSmith::new()
            .file(b"big.bin", &[7u8; 4000])
            .build();
        let gz = gzip(&tar);
        // Cuts the gz in half: the decoder can't deliver the 4000
        // decompressed bytes `skip` asks for.
        let truncated = gz[..gz.len() / 2].to_vec();

        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        read_entry_gz(Cursor::new(truncated), 3_900, 10, &tx);
        drop(tx);

        match rx.blocking_recv() {
            Some(Err(Error::Corrupt)) => {}
            other => {
                panic!("expected EXACTLY one Err(Corrupt) for EOF during discard, was {other:?}")
            }
        }
        assert!(
            rx.blocking_recv().is_none(),
            "not a single byte of data after the premature EOF: never silently short"
        );
    }

    /// A reader that arms `cancel` (the SAME one `build_index_gz` gets)
    /// after serving its first `arm_after` RAW (compressed) bytes:
    /// simulates a real cancellation IN THE MIDDLE of the decompression
    /// pipeline — unlike the `cancellation_cuts_indexing_short` test
    /// above, which arms the flag BEFORE starting (cuts short on the
    /// VERY FIRST read, before the build has made any progress at all).
    /// FIX-4 (rust MINOR-3a, #55 review).
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

    /// FIX-4 (rust MINOR-3a, #55 review): cancellation AFTER the pipeline
    /// has already served real bytes (not before the build starts) — the
    /// result is still `Cancelled`, NEVER `Corrupt`. Also validates
    /// FIX-3's `source()` chain: the signal crosses flate2 + tar-rs
    /// without losing its identity, even if it gets re-wrapped along the way.
    #[test]
    fn cancellation_mid_pipeline_is_cancelled_not_corrupt() {
        // HIGH-entropy content (xorshift32, not a periodic pattern):
        // deflate can't compress genuine noise, so the resulting gz is
        // ~proportional to the decompressed size — this keeps the WHOLE
        // gz from fitting in a single internal flate2 buffer (which would
        // let `served` jump from 0 to the total in a single read and lose
        // the "mid-way" nuance).
        let mut state: u32 = 0x2545_F491;
        let content: Vec<u8> = (0..2_000_000u32)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                (state & 0xFF) as u8
            })
            .collect();
        let tar = norte_testkit::TarSmith::new()
            .file(b"big.bin", &content)
            .file(b"second.bin", b"x")
            .build();
        let gz = gzip(&tar);
        let gz_len = gz.len() as u64;
        assert!(
            gz_len > 100_000,
            "poorly compressible content: the gz must still be large"
        );

        let cancel = Arc::new(AtomicBool::new(false));
        let reader = ArmCancelAfter {
            inner: Cursor::new(gz),
            served: 0,
            arm_after: gz_len / 2, // mid-way through the compressed stream
            cancel: Arc::clone(&cancel),
        };
        let got = build_index_gz(reader, (Some(0), Some(1)), &limits(), &cancel);
        assert_eq!(
            got.map(|_| ()).unwrap_err(),
            Error::Cancelled,
            "cancellation mid-pipeline: Cancelled, NOT Corrupt"
        );
    }
}
