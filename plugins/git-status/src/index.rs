//! Git's index (`.git/index`), read as bytes.
//!
//! This is the file that makes this column cheap: for every tracked path it
//! brings the `stat` git last saw. Comparing that against the current
//! `stat` answers "did it change?" without opening a single file — which is
//! exactly what a panel can afford to do per page.
//!
//! Format: `DIRC` header, version, entry count, and then the entries with
//! their fields in big-endian. What is implemented here is versions **2 and
//! 3**; 4 compresses names against the previous entry and is REJECTED BY
//! NAME, because reading it as if it were v2 would give made-up paths.

extern crate alloc;

use alloc::vec::Vec;

/// An index entry: the path and the `stat` git saved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexEntry {
    /// Path relative to the repository's root, in BYTES (hard rule 1).
    pub path: Vec<u8>,
    /// `st_mtime` in seconds, as git saved it.
    pub mtime_sec: u32,
    /// `st_mtime`, nanoseconds.
    pub mtime_nsec: u32,
    /// `st_ctime` in seconds.
    pub ctime_sec: u32,
    /// `st_ctime`, nanoseconds.
    pub ctime_nsec: u32,
    /// Size git saw (truncated to 32 bits by the format).
    pub size: u32,
    /// Inode, or 0 if git did not save it (Windows repositories).
    pub ino: u32,
    /// Device, or 0.
    pub dev: u32,
    /// File mode.
    pub mode: u32,
    /// Object id of the blob git has on record. This is what breaks the
    /// "racy" tie, where `stat` says nothing.
    pub oid: [u8; 20],
    /// `true` if the entry is in a merge-conflict state (stage other than
    /// 0). A file in conflict is not "modified".
    pub conflicted: bool,
}

/// Why an index could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IndexError {
    /// Does not start with `DIRC`.
    NotAnIndex,
    /// A version this parser does not read. 4 compresses path prefixes.
    UnsupportedVersion(u32),
    /// The file ended mid-entry.
    Truncated,
}

/// The already-parsed index. Entries stay in the file's order, which git
/// keeps SORTED by path — and that is what makes asking for a prefix cheap.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct GitIndex {
    entries: Vec<IndexEntry>,
}

impl GitIndex {
    /// Parses `.git/index`.
    ///
    /// # Errors
    ///
    /// [`IndexError`] depending on what fails. The extensions that follow
    /// the entries are ignored: the entry count is read from the header and
    /// it stops there.
    pub fn parse(raw: &[u8]) -> Result<Self, IndexError> {
        if raw.len() < 12 || &raw[..4] != b"DIRC" {
            return Err(IndexError::NotAnIndex);
        }
        let version = be32(&raw[4..8]);
        if version != 2 && version != 3 {
            return Err(IndexError::UnsupportedVersion(version));
        }
        let count = be32(&raw[8..12]) as usize;
        let mut entries = Vec::with_capacity(count.min(4_096));
        let mut at = 12;
        for _ in 0..count {
            let (entry, next) = parse_entry(raw, at, version)?;
            entries.push(entry);
            at = next;
        }
        Ok(Self { entries })
    }

    /// How many entries it brings.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// `true` if it brings none (a freshly created repository).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Entry `n`, in the file's order.
    #[must_use]
    pub fn entry(&self, n: usize) -> Option<&IndexEntry> {
        self.entries.get(n)
    }

    /// The entries whose path starts with `prefix`.
    ///
    /// Cheap because the index is SORTED: the first candidate is found by
    /// bisection and it continues while the prefix holds, instead of
    /// walking a hundred-thousand-entry index for every twenty-item page.
    pub fn under_prefix<'a>(&'a self, prefix: &'a [u8]) -> impl Iterator<Item = &'a IndexEntry> {
        let start = self.entries.partition_point(|e| e.path < prefix.to_vec());
        self.entries[start..]
            .iter()
            .take_while(move |e| e.path.starts_with(prefix))
    }

    /// The entry for an exact path, by bisection.
    #[must_use]
    pub fn get(&self, path: &[u8]) -> Option<&IndexEntry> {
        let at = self.entries.partition_point(|e| e.path.as_slice() < path);
        self.entries.get(at).filter(|e| e.path == path)
    }
}

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

/// A v2/v3 entry and the next one's offset.
///
/// Entries are aligned to 8 bytes with NUL padding (v2/v3), so the advance
/// is not "what the name took up" but that rounded up.
fn parse_entry(raw: &[u8], at: usize, version: u32) -> Result<(IndexEntry, usize), IndexError> {
    // 62 fixed bytes + name + padding; v3 adds 2 more bytes of flags.
    const FIXED: usize = 62;
    let extra = usize::from(version >= 3);
    if raw.len() < at + FIXED {
        return Err(IndexError::Truncated);
    }
    let f = &raw[at..];
    let flags = u16::from_be_bytes([f[60], f[61]]);
    // Bit 14 = extended (v3): there are 2 more bytes of flags before the name.
    let extended = usize::from(extra == 1 && flags & 0x4000 != 0) * 2;
    // The low 12 bits are the name's length, with 0xFFF = "longer than
    // that, look for the NUL".
    let name_len = usize::from(flags & 0x0FFF);
    // Bits 12-13 = stage: other than 0 is a merge conflict.
    let conflicted = (flags >> 12) & 0x3 != 0;
    let name_at = at + FIXED + extended;
    if raw.len() < name_at {
        return Err(IndexError::Truncated);
    }
    let name_end = if name_len == 0x0FFF {
        name_at
            + raw[name_at..]
                .iter()
                .position(|b| *b == 0)
                .ok_or(IndexError::Truncated)?
    } else {
        let end = name_at + name_len;
        if raw.len() < end {
            return Err(IndexError::Truncated);
        }
        end
    };
    let entry = IndexEntry {
        path: raw[name_at..name_end].to_vec(),
        ctime_sec: be32(&f[0..4]),
        ctime_nsec: be32(&f[4..8]),
        mtime_sec: be32(&f[8..12]),
        mtime_nsec: be32(&f[12..16]),
        dev: be32(&f[16..20]),
        ino: be32(&f[20..24]),
        mode: be32(&f[24..28]),
        size: be32(&f[36..40]),
        oid: {
            let mut oid = [0u8; 20];
            oid.copy_from_slice(&f[40..60]);
            oid
        },
        conflicted,
    };
    // Padding up to a multiple of 8, counting from the entry's start.
    let used = name_end - at;
    let padded = used + (8 - used % 8);
    Ok((entry, at + padded))
}

/// The index forge used by THIS module's tests and `status`'s.
/// Lives outside `mod tests` so another module can use it without
/// duplicating the format — which is exactly what would make the two copies
/// diverge.
#[cfg(test)]
pub mod tests_support {
    use super::*;

    /// A v2 index with `(path, size, mtime, oid)` per entry.
    #[must_use]
    pub fn forja(entries: &[(&[u8], u32, u32, [u8; 20])]) -> Vec<u8> {
        let with_mode: Vec<_> = entries
            .iter()
            .map(|(n, s, m, o)| (*n, *s, *m, *o, 0o100_644u32))
            .collect();
        forja_con_modo(&with_mode)
    }

    /// [`forja`] with each entry's MODE, which is the only thing that tells
    /// a submodule (`0o160000`, the "gitlink") apart from a normal file.
    #[must_use]
    pub fn forja_con_modo(entries: &[(&[u8], u32, u32, [u8; 20], u32)]) -> Vec<u8> {
        let mut out = b"DIRC".to_vec();
        out.extend_from_slice(&2u32.to_be_bytes());
        out.extend_from_slice(&(entries.len() as u32).to_be_bytes());
        for (name, size, mtime, oid, mode) in entries {
            let start = out.len();
            out.extend_from_slice(&7u32.to_be_bytes());
            out.extend_from_slice(&0u32.to_be_bytes());
            out.extend_from_slice(&mtime.to_be_bytes());
            out.extend_from_slice(&0u32.to_be_bytes());
            out.extend_from_slice(&3u32.to_be_bytes());
            out.extend_from_slice(&5u32.to_be_bytes());
            out.extend_from_slice(&mode.to_be_bytes());
            out.extend_from_slice(&0u32.to_be_bytes());
            out.extend_from_slice(&0u32.to_be_bytes());
            out.extend_from_slice(&size.to_be_bytes());
            out.extend_from_slice(oid);
            let len = u16::try_from(name.len()).unwrap_or(0x0FFF).min(0x0FFF);
            out.extend_from_slice(&len.to_be_bytes());
            out.extend_from_slice(name);
            let used = out.len() - start;
            out.extend(core::iter::repeat_n(0u8, 8 - used % 8));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Forges a v2 index with the given entries: `DIRC` header, version and
    /// count, and each entry with its 62 fixed bytes, the name and the
    /// padding.
    fn v2_index_with(entries: &[(&[u8], u32)]) -> Vec<u8> {
        forge(2, entries, 0)
    }

    fn forge(version: u32, entries: &[(&[u8], u32)], stage: u16) -> Vec<u8> {
        let mut out = b"DIRC".to_vec();
        out.extend_from_slice(&version.to_be_bytes());
        out.extend_from_slice(&(entries.len() as u32).to_be_bytes());
        for (name, size) in entries {
            let start = out.len();
            out.extend_from_slice(&7u32.to_be_bytes()); // ctime sec
            out.extend_from_slice(&0u32.to_be_bytes()); // ctime nsec
            out.extend_from_slice(&11u32.to_be_bytes()); // mtime sec
            out.extend_from_slice(&0u32.to_be_bytes()); // mtime nsec
            out.extend_from_slice(&3u32.to_be_bytes()); // dev
            out.extend_from_slice(&5u32.to_be_bytes()); // ino
            out.extend_from_slice(&0o100_644u32.to_be_bytes()); // mode
            out.extend_from_slice(&0u32.to_be_bytes()); // uid
            out.extend_from_slice(&0u32.to_be_bytes()); // gid
            out.extend_from_slice(&size.to_be_bytes()); // size
            out.extend_from_slice(&[0u8; 20]); // sha1
            let len = u16::try_from(name.len()).unwrap_or(0x0FFF).min(0x0FFF);
            out.extend_from_slice(&(len | (stage << 12)).to_be_bytes());
            out.extend_from_slice(name);
            let used = out.len() - start;
            out.extend(std::iter::repeat_n(0u8, 8 - used % 8));
        }
        out
    }

    #[test]
    fn parses_v2_and_keeps_the_names_bytes() {
        let raw = v2_index_with(&[(b"cp437-\xa4\xa5.txt", 3), (b"src/lib.rs", 10)]);
        let idx = GitIndex::parse(&raw).unwrap();
        assert_eq!(idx.len(), 2);
        assert_eq!(idx.entry(0).unwrap().path, b"cp437-\xa4\xa5.txt");
        assert_eq!(idx.entry(1).unwrap().size, 10);
        assert_eq!(idx.entry(1).unwrap().mtime_sec, 11);
        assert_eq!(idx.entry(1).unwrap().ino, 5);
    }

    #[test]
    fn version_4_is_rejected_by_name() {
        let mut raw = v2_index_with(&[(b"a", 1)]);
        raw[7] = 4;
        assert_eq!(
            GitIndex::parse(&raw),
            Err(IndexError::UnsupportedVersion(4)),
            "v4 compresses path prefixes; saying so beats reading garbage"
        );
    }

    #[test]
    fn something_that_is_not_an_index_says_so() {
        assert_eq!(GitIndex::parse(b"nope"), Err(IndexError::NotAnIndex));
        assert_eq!(GitIndex::parse(&[]), Err(IndexError::NotAnIndex));
    }

    #[test]
    fn a_truncated_index_does_not_invent_entries() {
        let raw = v2_index_with(&[(b"a.txt", 1), (b"b.txt", 1)]);
        assert_eq!(
            GitIndex::parse(&raw[..raw.len() - 10]),
            Err(IndexError::Truncated)
        );
    }

    #[test]
    fn the_index_is_sorted_and_that_is_what_makes_the_prefix_cheap() {
        let raw = v2_index_with(&[(b"a/b.txt", 1), (b"a/c.txt", 1), (b"z.txt", 1)]);
        let idx = GitIndex::parse(&raw).unwrap();
        assert_eq!(idx.under_prefix(b"a/").count(), 2);
        assert_eq!(idx.under_prefix(b"").count(), 3);
        assert_eq!(idx.under_prefix(b"q").count(), 0);
    }

    #[test]
    fn get_finds_an_exact_path_and_not_its_prefix() {
        let raw = v2_index_with(&[(b"a/b.txt", 1), (b"ab.txt", 2)]);
        let idx = GitIndex::parse(&raw).unwrap();
        assert_eq!(idx.get(b"ab.txt").unwrap().size, 2);
        assert!(idx.get(b"a/").is_none(), "a prefix is not an entry");
    }

    /// A stage other than 0 = merge conflict. A file in conflict is not
    /// "modified", and calling it that would hide what is actually
    /// happening.
    #[test]
    fn a_conflicted_entry_is_marked() {
        let raw = forge(2, &[(b"contested.txt", 1)], 2);
        let idx = GitIndex::parse(&raw).unwrap();
        assert!(idx.entry(0).unwrap().conflicted);
    }
}
