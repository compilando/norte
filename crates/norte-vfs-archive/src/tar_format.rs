//! Indexing of plain tar (ustar/GNU/pax via the `tar` crate). SYNC: runs in
//! `spawn_blocking` over a [`ProviderReader`](crate::blocking::ProviderReader).

use std::io::{Read, Seek};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use norte_proto::{EntryKind, Error};

use crate::index::{ArchiveIndex, Limits, Locator, Node};

/// Milliseconds since epoch from the seconds (saturating: garbage mtimes
/// from hostile tars don't panic). Debt: pax mtime overrides aren't
/// applied (the `tar` crate only overwrites size/uid/gid) — the mtime
/// shown is the ustar header's.
fn secs_to_ms(secs: u64) -> Option<i64> {
    i64::try_from(secs).ok()?.checked_mul(1000)
}

/// Shape of a tar entry classified by its HEADERS (without touching the
/// data): shared by the plain index (`entries_with_seek`, this module) and
/// tar.gz's (`entries()`, [`targz_format`](crate::targz_format) — ADR
/// 0028, #55). Each builder decides a `File`'s `Locator` — contiguous
/// (plain) or DECOMPRESSED offset (gz) — and validates its own budget; the
/// rest (dir/symlink/other, name, mtime, `pax_global_header` filter) is
/// IDENTICAL between both formats.
pub(crate) enum EntryShape {
    /// Directory.
    Dir,
    /// Symlink; raw target (`None` if the header doesn't carry one).
    Symlink(Option<Vec<u8>>),
    /// Regular file. `offset` is within the stream the `Archive` is
    /// walking (container bytes on plain tar; DECOMPRESSED bytes on
    /// tar.gz — interpreting it is the caller's job).
    File { offset: u64, size: u64 },
    /// Hardlinks, devices, fifos, GNU sparse…: no locator (read →
    /// `Unsupported`).
    Other,
}

/// Classifies a tar entry by its headers. `None` = meta already consumed
/// by the iterator that must be skipped from the index (`g` =
/// `pax_global_header`: the `tar` crate's iterator consumes L/K/x
/// automatically but NOT `g` — without this filter, every `git archive`
/// tar would list a phantom `pax_global_header`, audit 8e H5).
pub(crate) fn classify_entry<R: Read>(
    entry: &tar::Entry<'_, R>,
) -> Option<(Vec<u8>, Option<i64>, EntryShape)> {
    if entry.header().entry_type() == tar::EntryType::XGlobalHeader {
        return None;
    }
    let raw_name = entry.path_bytes().to_vec();
    let header = entry.header();
    let kind = header.entry_type();
    let mtime_ms = header.mtime().ok().and_then(secs_to_ms);
    let shape = match kind {
        tar::EntryType::Directory => EntryShape::Dir,
        tar::EntryType::Symlink => EntryShape::Symlink(entry.link_name_bytes().map(|b| b.to_vec())),
        tar::EntryType::Regular | tar::EntryType::Continuous => EntryShape::File {
            offset: entry.raw_file_position(),
            size: entry.size(),
        },
        _ => EntryShape::Other,
    };
    Some((raw_name, mtime_ms, shape))
}

/// Builds the index by walking the tar's HEADERS (`entries_with_seek`: the
/// data is skipped with `Seek`, not downloaded — over a remote provider
/// the cost is O(headers), not O(size)).
///
/// `cancel` is checked in the inner loop (rule 3): the async side arms it
/// by dropping the future and the blocking thread ends at the next entry.
/// `container_len` validates every locator against the real size: a
/// truncated tar can't promise data beyond the container.
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
            tracing::debug!("tar indexing cancelled");
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
            EntryShape::File { offset, size } => {
                if offset
                    .checked_add(size)
                    .is_none_or(|end| end > container_len)
                {
                    // With seek the data isn't read: a truncated tar is
                    // detected by validating the locator, not by
                    // stumbling into EOF.
                    tracing::warn!("truncated tar: entry promises data beyond the container");
                    return Err(Error::Corrupt);
                }
                Node {
                    kind: EntryKind::File,
                    size: Some(size),
                    mtime_ms,
                    locator: Some(Locator::Tar { offset, size }),
                    link_target: None,
                    zip: None,
                }
            }
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
        // Omitted entries also spend budget: a tar with millions of
        // hostile names doesn't iterate for free (phase 8d finding M2).
        if index.skipped > limits.max_entries as u64 {
            tracing::warn!(
                max = limits.max_entries,
                "tar exceeds the omitted-entries budget"
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

fn corrupt(e: &std::io::Error) -> Error {
    // Genuine IO from the inner provider (a network drop mid-parse): it's
    // propagated VERBATIM, never disguised as Corrupt (#58).
    if let Some(inner) = crate::blocking::inner_proto_error(e) {
        return inner;
    }
    tracing::warn!(error = %e, "corrupt or unreadable tar");
    Error::Corrupt
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn limits() -> Limits {
        Limits::default()
    }

    /// Cancellation cuts the loop at the next entry (rule 3).
    #[test]
    fn cancellation_cuts_indexing_short() {
        let mut smith = norte_testkit::TarSmith::new();
        for i in 0..50u32 {
            smith = smith.file(format!("f{i}").as_bytes(), b"x");
        }
        let bytes = smith.build();
        let len = bytes.len() as u64;
        let cancel = Arc::new(AtomicBool::new(true)); // armed BEFORE
        let got = build_index(
            Cursor::new(bytes),
            len,
            (Some(0), Some(len)),
            &limits(),
            &cancel,
        );
        assert_eq!(got.map(|_| ()).unwrap_err(), Error::Cancelled);
    }

    /// A locator promising data outside the container = truncated.
    #[test]
    fn a_locator_outside_the_container_is_corrupt() {
        let bytes = norte_testkit::TarSmith::new()
            .file(b"big.bin", &[7u8; 2000])
            .build();
        let len = bytes.len() as u64;
        let cancel = Arc::new(AtomicBool::new(false));
        // We lie: the container "measures" less than what the header promises.
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
