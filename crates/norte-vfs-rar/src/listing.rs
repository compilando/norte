//! Parses what the delegate PRINTED. Pure over bytes: it spawns no
//! processes, so the encoding rules are tested on a machine with neither
//! `7z` nor `unrar` installed.
//!
//! Names are [`Vec<u8>`] and never `String` (rule 1): the bytes having
//! arrived through a pipe does not make them UTF-8.

/// An entry exactly as the delegate printed it, still NOT validated as a
/// `VPath` segment — that is the index's job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawEntry {
    /// Raw bytes of the name, exactly as they came out of the pipe.
    pub name: Vec<u8>,
    /// Uncompressed size in bytes.
    pub size: u64,
    /// The entry is a directory.
    pub is_dir: bool,
    /// mtime in epoch seconds, if the delegate printed it and it was
    /// readable.
    pub mtime: Option<i64>,
    /// The entry is encrypted (reading it would ask for a password; the
    /// runner NEVER lets it be asked).
    pub encrypted: bool,
    /// The entry is part of a solid block.
    pub solid: bool,
}

/// The result of a parse: readable entries and **how many were skipped**.
///
/// Counting the skipped ones is the same "skip WITH a signal" contract as
/// ADR 0018: an archive with a weird entry is still explored, but the user
/// finds out.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Listing {
    /// The entries that could be read.
    pub entries: Vec<RawEntry>,
    /// How many records were discarded for not being interpretable.
    pub skipped: u64,
}

/// A record under construction: key/value pairs in bytes.
#[derive(Default)]
struct Record<'a> {
    fields: Vec<(&'a [u8], &'a [u8])>,
    malformed: bool,
}

impl<'a> Record<'a> {
    fn is_empty(&self) -> bool {
        self.fields.is_empty() && !self.malformed
    }

    fn get(&self, key: &[u8]) -> Option<&'a [u8]> {
        self.fields
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| *v)
    }
}

/// Splits a line into `key<sep>value`, with the separator already chosen by
/// the format. The key is trimmed on both sides; the value only on the left
/// **one** space and on the right a `\r`: a name can end in a space and
/// losing it would show a file that is not that file.
fn split_field<'a>(line: &'a [u8], sep: &[u8]) -> Option<(&'a [u8], &'a [u8])> {
    let (key, value) = match line.windows(sep.len()).position(|w| w == sep) {
        Some(at) => (&line[..at], &line[at + sep.len()..]),
        // `Created =` with no value: 7z prints the key and nothing after it.
        None => (line.strip_suffix(trim_ascii(sep))?, &line[line.len()..]),
    };
    let key = trim_ascii(key);
    // Real keys carry spaces (`Packed Size`, `Host OS`, `NT Security`):
    // requiring a single word discarded half of 7z's output. What IS
    // required is that it starts with a letter and carries nothing that a
    // continued filename would carry.
    if !key.first().is_some_and(u8::is_ascii_alphabetic)
        || key
            .iter()
            .any(|b| !(b.is_ascii_alphanumeric() || *b == b' '))
    {
        return None;
    }
    Some((key, value))
}

fn trim_ascii(mut s: &[u8]) -> &[u8] {
    while let [first, rest @ ..] = s {
        if first.is_ascii_whitespace() {
            s = rest;
        } else {
            break;
        }
    }
    while let [rest @ .., last] = s {
        if last.is_ascii_whitespace() {
            s = rest;
        } else {
            break;
        }
    }
    s
}

/// Walks `stdout` by lines, grouping records separated by a blank line, and
/// calls `emit` with each one. A record with some line that does not parse
/// as a field is marked `malformed`.
fn for_each_record<'a>(stdout: &'a [u8], sep: &[u8], mut emit: impl FnMut(&Record<'a>)) {
    let mut current = Record::default();
    for raw in stdout.split(|b| *b == b'\n') {
        let line = raw.strip_suffix(b"\r").unwrap_or(raw);
        if trim_ascii(line).is_empty() {
            if !current.is_empty() {
                emit(&current);
            }
            current = Record::default();
            continue;
        }
        match split_field(line, sep) {
            Some((k, v)) => current.fields.push((k, v)),
            None => current.malformed = true,
        }
    }
    if !current.is_empty() {
        emit(&current);
    }
}

fn parse_u64(v: &[u8]) -> u64 {
    std::str::from_utf8(trim_ascii(v))
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

/// `YYYY-MM-DD HH:MM:SS[,fraction]` (the delegate's local time, which is the
/// only thing it prints) to epoch seconds. Returns `None` on any deviation:
/// a made-up mtime is worse than none.
fn parse_timestamp(value: &[u8]) -> Option<i64> {
    let text = std::str::from_utf8(trim_ascii(value)).ok()?;
    let (date, time) = text.split(',').next()?.split_once(' ')?;
    let mut fields = date.split('-');
    let (year, month, day): (i64, i64, i64) = (
        fields.next()?.parse().ok()?,
        fields.next()?.parse().ok()?,
        fields.next()?.parse().ok()?,
    );
    if fields.next().is_some() {
        return None;
    }
    let mut fields = time.split(':');
    let (hour, minute, second): (i64, i64, i64) = (
        fields.next()?.parse().ok()?,
        fields.next()?.parse().ok()?,
        fields.next()?.parse().ok()?,
    );
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    Some(days_from_civil(year, month, day) * 86_400 + hour * 3_600 + minute * 60 + second)
}

/// Days since 1970-01-01 (Hinnant's `days_from_civil` algorithm, public
/// domain). Implemented here so as not to drag in a whole date dependency
/// for a listing's `Modified =`.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let shifted_month = (month + 9) % 12;
    let day_of_year = (153 * shifted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// Parses the output of `7z l -slt -- <archive>`.
///
/// The header block describes the ARCHIVE (its first `Path =` is the
/// `.rar`, not an entry) and ends at the dashes line; everything before it
/// is ignored without counting it as skipped.
///
/// ```
/// let out = norte_vfs_rar::parse_7z_slt(b"----------\nPath = a.txt\nSize = 3\n");
/// assert_eq!(out.entries[0].name, b"a.txt");
/// ```
#[must_use]
pub fn parse_7z_slt(stdout: &[u8]) -> Listing {
    // Without the dashes line there is no listing to read: 7z never got started.
    let Some(body) = stdout
        .windows(10)
        .position(|w| w == b"----------")
        .map(|at| &stdout[at + 10..])
    else {
        return Listing::default();
    };
    let mut out = Listing::default();
    for_each_record(body, b" = ", |rec| {
        let Some(name) = rec.get(b"Path").filter(|_| !rec.malformed) else {
            out.skipped += 1;
            return;
        };
        let attrs = rec.get(b"Attributes").unwrap_or_default();
        out.entries.push(RawEntry {
            name: name.to_vec(),
            size: rec.get(b"Size").map_or(0, parse_u64),
            is_dir: rec.get(b"Folder").is_some_and(|v| trim_ascii(v) == b"+")
                || attrs.starts_with(b"D"),
            mtime: rec.get(b"Modified").and_then(parse_timestamp),
            encrypted: rec.get(b"Encrypted").is_some_and(|v| trim_ascii(v) == b"+"),
            solid: rec.get(b"Solid").is_some_and(|v| trim_ascii(v) != b"-"),
        });
    });
    out
}

/// Parses the output of `unrar vt -- <archive>`.
///
/// A record WITHOUT `Name:` is the archive's header (`Archive:`,
/// `Details:`), not a lost entry: it is ignored without counting it.
///
/// ```
/// let out = norte_vfs_rar::parse_unrar_vt(b"\n        Name: a.txt\n        Type: File\n");
/// assert_eq!(out.entries[0].name, b"a.txt");
/// ```
#[must_use]
pub fn parse_unrar_vt(stdout: &[u8]) -> Listing {
    let mut out = Listing::default();
    for_each_record(stdout, b":", |rec| {
        // Without `Name:` the record is the header (unrar's banner, the
        // `Archive:`/`Details:`), not a lost entry: ignore without counting.
        let Some(name) = rec.get(b"Name") else {
            return;
        };
        if rec.malformed {
            out.skipped += 1;
            return;
        }
        let flags = rec.get(b"Flags").unwrap_or_default();
        out.entries.push(RawEntry {
            name: trim_leading_space(name).to_vec(),
            size: rec.get(b"Size").map_or(0, parse_u64),
            is_dir: rec
                .get(b"Type")
                .is_some_and(|v| trim_ascii(v).eq_ignore_ascii_case(b"Directory")),
            mtime: rec.get(b"mtime").and_then(parse_timestamp),
            encrypted: contains(flags, b"encrypted"),
            solid: contains(flags, b"solid"),
        });
    });
    out
}

/// Strips ONE leading space (the `key: value`'s), not any the name might
/// genuinely carry.
fn trim_leading_space(v: &[u8]) -> &[u8] {
    v.strip_prefix(b" ").unwrap_or(v)
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|w| w.eq_ignore_ascii_case(needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real output from `7z l -slt` (7-Zip 26.02) trimmed to two entries.
    const SEVENZ_SLT: &[u8] = b"\
Listing archive: t.rar\n\
\n\
--\n\
Path = t.rar\n\
Type = Rar5\n\
Solid = -\n\
\n\
----------\n\
Path = hello.txt\n\
Folder = -\n\
Size = 11\n\
Modified = 2021-01-14 10:25:36\n\
Encrypted = -\n\
Solid = -\n\
CRC = DB187CE4\n\
\n\
Path = cp437-\xa4\xa5.txt\n\
Folder = -\n\
Size = 16\n\
Modified = 2021-01-14 10:25:36\n\
Encrypted = -\n\
Solid = -\n\
";

    #[test]
    fn slt_ignores_the_archives_header_and_keeps_raw_bytes() {
        let out = parse_7z_slt(SEVENZ_SLT);
        // `Path = t.rar` is the archive itself, not an entry: it comes
        // before the `----------`.
        assert_eq!(out.entries.len(), 2, "the header is not an entry");
        assert_eq!(out.entries[0].name, b"hello.txt");
        assert_eq!(out.entries[0].size, 11);
        assert_eq!(
            out.entries[1].name, b"cp437-\xa4\xa5.txt",
            "raw bytes, not lossy"
        );
        assert_eq!(out.skipped, 0);
    }

    #[test]
    fn slt_reads_an_encrypted_solid_directory() {
        let raw = b"----------\nPath = d\nFolder = +\nSize = 0\nEncrypted = +\nSolid = +\n";
        let out = parse_7z_slt(raw);
        let e = &out.entries[0];
        assert!(e.is_dir && e.encrypted && e.solid);
    }

    #[test]
    fn slt_converts_modified_to_epoch() {
        let raw = b"----------\nPath = a\nSize = 0\nModified = 1970-01-02 00:00:01\n";
        assert_eq!(parse_7z_slt(raw).entries[0].mtime, Some(86_401));
    }

    /// Real output from `unrar vt` (UNRAR 7.23). The non-UTF8 name arrives
    /// TRUNCATED by unrar itself: `cp437-` with no extension. Not a parser
    /// bug, and the reason 7z goes first.
    const UNRAR_VT: &[u8] = b"\
\n\
Archive: t.rar\n\
Details: RAR 5\n\
\n\
        Name: hello.txt\n\
        Type: File\n\
        Size: 11\n\
       mtime: 2021-01-14 09:25:36,000000000\n\
  Attributes: ----r-----\n\
\n\
        Name: dir/nested.txt\n\
        Type: Directory\n\
        Size: 0\n\
       mtime: 2021-01-14 09:25:36,000000000\n\
  Attributes: ----r-----\n\
";

    #[test]
    fn vt_reads_name_type_and_size() {
        let out = parse_unrar_vt(UNRAR_VT);
        assert_eq!(out.entries.len(), 2);
        assert_eq!(out.entries[0].name, b"hello.txt");
        assert!(!out.entries[0].is_dir);
        assert!(out.entries[1].is_dir, "Type: Directory");
        assert_eq!(out.skipped, 0, "the archive's header is not a skip");
    }

    #[test]
    fn vt_does_not_trim_a_trailing_space_off_the_name() {
        let out = parse_unrar_vt(b"\n        Name: weird \n        Type: File\n");
        assert_eq!(out.entries[0].name, b"weird ", "the name ends in a space");
    }

    /// Regression measured against 7-Zip 26.02: half of a real output
    /// carries keys WITH SPACES (`Packed Size`, `Host OS`, `NT Security`)
    /// and keys with an EMPTY value (`Created =`). Requiring a single-word
    /// key marked every record as unreadable and the listing came out
    /// empty.
    #[test]
    fn slt_keys_with_spaces_and_empty_value_do_not_break_the_record() {
        let raw = b"----------\nPath = a.txt\nFolder = -\nSize = 8\nPacked Size = 8\n\
Created = \nAccessed =\nHost OS = Unix\nNT Security = \n";
        let out = parse_7z_slt(raw);
        assert_eq!(out.skipped, 0, "none of those lines is unreadable");
        assert_eq!(out.entries.len(), 1);
        assert_eq!(out.entries[0].size, 8);
    }

    /// unrar's banner (`UNRAR 7.23 freeware  Copyright (c) …`) does not parse
    /// as a field and is NOT a lost entry: it is ignored without counting
    /// it.
    #[test]
    fn vt_the_banner_does_not_count_as_skipped() {
        let raw = b"\nUNRAR 7.23 freeware      Copyright (c) 1993-2026 Alexander Roshal\n\
\nArchive: t.rar\nDetails: RAR 5\n\n        Name: a.txt\n        Type: File\n";
        let out = parse_unrar_vt(raw);
        assert_eq!(out.entries.len(), 1);
        assert_eq!(out.skipped, 0);
    }

    #[test]
    fn a_name_with_a_line_break_is_skipped_and_counted() {
        // A line-based output cannot carry a `\n` inside a name without
        // guessing. Guessing here means showing a file that is not that
        // file.
        let raw = b"----------\nPath = ok.txt\nSize = 1\n\nPath = bad\nname.txt\nSize = 2\n";
        let out = parse_7z_slt(raw);
        assert_eq!(out.entries.len(), 1);
        assert_eq!(out.skipped, 1, "skipped and COUNTED, as ADR 0018 requires");
    }

    #[test]
    fn vt_an_unreadable_record_is_skipped_and_counted() {
        let raw = b"\n        Name: bad\nname.txt\n        Type: File\n";
        let out = parse_unrar_vt(raw);
        assert!(out.entries.is_empty());
        assert_eq!(out.skipped, 1);
    }

    #[test]
    fn without_a_separator_the_listing_is_empty_and_counts_no_skips() {
        let out = parse_7z_slt(b"ERROR: cannot open t.rar\n");
        assert_eq!(out, Listing::default());
    }
}
