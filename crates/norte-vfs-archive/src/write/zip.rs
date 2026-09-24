//! INCREMENTAL zip writer: entry by entry, chunk by chunk.
//!
//! Writes nowhere — it produces bytes in a buffer the caller drains and
//! sends wherever it likes (a remote `ByteSink`, a local file). That's
//! what allows packing against an async destination without holding the
//! whole archive in memory, and it also makes it testable without I/O.
//!
//! Only what this repository knows how to read: `store` and `deflate`, no encryption.

use std::io::Write as _;

use super::{PackEntry, PackError};

/// Signature of an entry's local header.
const LOCAL_SIG: u32 = 0x0403_4b50;
/// Signature of a central directory entry.
const CD_SIG: u32 = 0x0201_4b50;
/// Signature of the End Of Central Directory.
const EOCD_SIG: u32 = 0x0605_4b50;
/// Signature of zip64's EOCD.
const EOCD64_SIG: u32 = 0x0606_4b50;
/// Signature of zip64's EOCD locator.
const EOCD64_LOC_SIG: u32 = 0x0706_4b50;
/// Signature of the data descriptor (optional, but everybody writes it).
const DD_SIG: u32 = 0x0807_4b50;

/// Flags bit 3: sizes and CRC go AFTER the data, in a descriptor.
///
/// It's what makes it possible to write without knowing in advance how
/// much the compressed entry is going to take up, which is exactly what
/// an incremental writer doesn't know.
const FLAG_DATA_DESCRIPTOR: u16 = 1 << 3;
/// Bit 11: the name is in UTF-8.
const FLAG_UTF8: u16 = 1 << 11;

/// `store` method (uncompressed).
const METHOD_STORE: u16 = 0;
/// `deflate` method.
const METHOD_DEFLATE: u16 = 8;

/// Minimum version to extract: 2.0 (deflate). With zip64 it goes up to 4.5.
const VERSION_BASE: u16 = 20;
/// Minimum version when the entry needs zip64.
const VERSION_ZIP64: u16 = 45;

/// From here on a 32-bit field isn't enough and zip64 is needed.
const U32_MAX: u64 = u32::MAX as u64;

/// An already-written entry, for the central directory.
struct Written {
    name: Vec<u8>,
    flags: u16,
    method: u16,
    dos_time: u16,
    dos_date: u16,
    crc: u32,
    comp_size: u64,
    uncomp_size: u64,
    offset: u64,
    external_attrs: u32,
}

/// What's being compressed right now.
enum Body {
    Store,
    Deflate(Box<flate2::write::DeflateEncoder<Vec<u8>>>),
}

/// The entry in progress.
struct InProgress {
    name: Vec<u8>,
    flags: u16,
    method: u16,
    dos_time: u16,
    dos_date: u16,
    offset: u64,
    external_attrs: u32,
    /// The local header announced zip64 (by the declared size).
    zip64: bool,
    crc: flate2::Crc,
    uncomp: u64,
    comp: u64,
    body: Body,
}

/// Incremental zip writer.
///
/// The cycle is `begin(entry)` → `data(chunk)`* → `end()` per entry, and
/// `finish()` when done. Between calls, [`ZipWriter::take`] pulls out what
/// was produced: whoever's packing drains it and sends it, so neither the
/// archive nor a whole entry lives in memory.
pub struct ZipWriter {
    out: Vec<u8>,
    /// Bytes already DELIVERED by `take`, which is what fixes the central
    /// directory's offsets: `out` gets emptied, the count doesn't.
    delivered: u64,
    done: Vec<Written>,
    current: Option<InProgress>,
    level: u32,
}

impl ZipWriter {
    /// A writer with the given compression level (0 = `store`).
    #[must_use]
    pub fn new(level: u32) -> Self {
        Self {
            out: Vec::new(),
            delivered: 0,
            done: Vec::new(),
            current: None,
            level,
        }
    }

    /// A writer that believes it has already delivered `pos` bytes.
    ///
    /// Exists for zip64 TESTS, and there's no other honest way: the case
    /// that matters — an archive that goes past 4 GiB with small members
    /// — reproduces with this seat in microseconds and with four
    /// gigabytes of disk nowhere.
    #[cfg(test)]
    #[must_use]
    pub(super) fn desde(pos: u64, level: u32) -> Self {
        let mut w = Self::new(level);
        w.delivered = pos;
        w
    }

    /// Bytes produced so far. Empties the buffer: the caller is who keeps them.
    pub fn take(&mut self) -> Vec<u8> {
        let v = std::mem::take(&mut self.out);
        self.delivered += v.len() as u64;
        v
    }

    /// How much has been produced in total (delivered + pending).
    fn pos(&self) -> u64 {
        self.delivered + self.out.len() as u64
    }

    /// Opens an entry.
    ///
    /// # Errors
    ///
    /// [`PackError::Nombre`] if the name doesn't fit the format's 16 bits,
    /// or if an entry is already open.
    pub fn begin(&mut self, entry: &PackEntry) -> Result<(), PackError> {
        if self.current.is_some() {
            return Err(PackError::Estado);
        }
        let mut name = entry.name.clone();
        if entry.dir && !name.ends_with(b"/") {
            // The trailing slash IS what says it's a directory in zip.
            name.push(b'/');
        }
        if u16::try_from(name.len()).is_err() {
            return Err(PackError::Nombre);
        }
        // **Bit 11 only if the name IS UTF-8** (rule 1). Our reader keeps
        // the raw bytes and doesn't look at the bit, so the round trip is
        // exact either way; the bit is written for OTHER programs, which
        // do decode by it — and setting it on a name that isn't UTF-8
        // would turn the user's name into replacement characters in any
        // unzip in the world.
        let utf8 = std::str::from_utf8(&name).is_ok();
        let method = if entry.dir || self.level == 0 {
            METHOD_STORE
        } else {
            METHOD_DEFLATE
        };
        let flags = FLAG_DATA_DESCRIPTOR | if utf8 { FLAG_UTF8 } else { 0 };
        let (dos_time, dos_date) = dos_datetime(entry.mtime_ms);
        let offset = self.pos();
        // **The LOCAL header has to say whether the entry is zip64**, and
        // it's known here: the size comes with the entry. A STREAMING
        // reader — `unzip` from a pipe, bsdtar, `zipfile` in stream mode
        // — hasn't seen the central directory yet, so it decides the data
        // descriptor's width from this. Without the marker it would read
        // 12 bytes where we write 20 and would desync on the first entry
        // over 4 GiB; the central directory saved it, and that's why the
        // round trip with our own reader never saw it.
        let is_zip64_entry = entry.size > U32_MAX;
        let mut local_extra: Vec<u8> = Vec::new();
        if is_zip64_entry {
            put_u16(&mut local_extra, 0x0001);
            put_u16(&mut local_extra, 16);
            // The REAL values aren't known yet (the descriptor carries
            // them); what matters is the width the record announces.
            put_u64(&mut local_extra, 0);
            put_u64(&mut local_extra, 0);
        }

        put_u32(&mut self.out, LOCAL_SIG);
        put_u16(
            &mut self.out,
            if is_zip64_entry {
                VERSION_ZIP64
            } else {
                VERSION_BASE
            },
        );
        put_u16(&mut self.out, flags);
        put_u16(&mut self.out, method);
        put_u16(&mut self.out, dos_time);
        put_u16(&mut self.out, dos_date);
        // CRC and sizes go to zero: the data descriptor carries them,
        // which is what bit 3 announces.
        put_u32(&mut self.out, 0);
        put_u32(&mut self.out, if is_zip64_entry { u32::MAX } else { 0 });
        put_u32(&mut self.out, if is_zip64_entry { u32::MAX } else { 0 });
        put_u16(&mut self.out, u16::try_from(name.len()).unwrap_or(u16::MAX));
        put_u16(&mut self.out, u16::try_from(local_extra.len()).unwrap_or(0));
        self.out.extend_from_slice(&name);
        self.out.extend_from_slice(&local_extra);

        let body = if method == METHOD_DEFLATE {
            Body::Deflate(Box::new(flate2::write::DeflateEncoder::new(
                Vec::new(),
                flate2::Compression::new(self.level),
            )))
        } else {
            Body::Store
        };
        self.current = Some(InProgress {
            name,
            flags,
            method,
            dos_time,
            dos_date,
            offset,
            external_attrs: external_attrs(entry),
            zip64: is_zip64_entry,
            crc: flate2::Crc::new(),
            uncomp: 0,
            comp: 0,
            body,
        });
        Ok(())
    }

    /// Adds data to the open entry.
    ///
    /// # Errors
    ///
    /// [`PackError::Estado`] with no entry open, [`PackError::Io`] if the
    /// compressor fails.
    pub fn data(&mut self, chunk: &[u8]) -> Result<(), PackError> {
        let current = self.current.as_mut().ok_or(PackError::Estado)?;
        current.crc.update(chunk);
        current.uncomp += chunk.len() as u64;
        match &mut current.body {
            Body::Store => {
                current.comp += chunk.len() as u64;
                self.out.extend_from_slice(chunk);
            }
            Body::Deflate(enc) => {
                enc.write_all(chunk).map_err(|_| PackError::Io)?;
                // Drains whatever the compressor has produced instead of
                // waiting for `finish`: otherwise a one-gigabyte entry
                // would live whole in the encoder's buffer.
                let ready = std::mem::take(enc.get_mut());
                current.comp += ready.len() as u64;
                self.out.extend_from_slice(&ready);
            }
        }
        Ok(())
    }

    /// Closes the open entry and writes its data descriptor.
    ///
    /// # Errors
    ///
    /// [`PackError::Estado`] with no entry open, [`PackError::Io`] if the
    /// compressor fails to finish.
    pub fn end(&mut self) -> Result<(), PackError> {
        let mut current = self.current.take().ok_or(PackError::Estado)?;
        if let Body::Deflate(enc) = &mut current.body {
            let tail = enc.try_finish().map(|()| std::mem::take(enc.get_mut()));
            let tail = tail.map_err(|_| PackError::Io)?;
            current.comp += tail.len() as u64;
            self.out.extend_from_slice(&tail);
        }
        let crc = std::mem::replace(&mut current.crc, flate2::Crc::new()).sum();
        // The descriptor's width is the one the local header ANNOUNCED,
        // not whatever the sizes turn out to be: a streaming reader
        // already decided by it, and changing our mind here is the
        // desync all over again.
        let zip64 = current.zip64 || current.comp > U32_MAX || current.uncomp > U32_MAX;
        put_u32(&mut self.out, DD_SIG);
        put_u32(&mut self.out, crc);
        if zip64 {
            put_u64(&mut self.out, current.comp);
            put_u64(&mut self.out, current.uncomp);
        } else {
            put_u32(
                &mut self.out,
                u32::try_from(current.comp).unwrap_or(u32::MAX),
            );
            put_u32(
                &mut self.out,
                u32::try_from(current.uncomp).unwrap_or(u32::MAX),
            );
        }
        self.done.push(Written {
            name: current.name,
            flags: current.flags,
            method: current.method,
            dos_time: current.dos_time,
            dos_date: current.dos_date,
            crc,
            comp_size: current.comp,
            uncomp_size: current.uncomp,
            offset: current.offset,
            external_attrs: current.external_attrs,
        });
        Ok(())
    }

    /// Writes the central directory and the EOCD. After this the archive
    /// is complete.
    ///
    /// # Errors
    ///
    /// [`PackError::Estado`] if an entry is still open.
    pub fn finish(&mut self) -> Result<(), PackError> {
        if self.current.is_some() {
            return Err(PackError::Estado);
        }
        let cd_offset = self.pos();
        let done = std::mem::take(&mut self.done);
        for e in &done {
            self.write_cd(e);
        }
        let cd_size = self.pos() - cd_offset;
        let n = done.len();
        // Zip64 when something doesn't fit in 32 bits: the entry count,
        // the directory's size, or where it starts. A silently truncated
        // offset is a corrupt archive that OPENS, which is worse than one
        // that doesn't.
        let needs_64 = n > usize::from(u16::MAX)
            || cd_size > U32_MAX
            || cd_offset > U32_MAX
            || done
                .iter()
                .any(|e| e.comp_size > U32_MAX || e.uncomp_size > U32_MAX || e.offset > U32_MAX);
        if needs_64 {
            let eocd64 = self.pos();
            put_u32(&mut self.out, EOCD64_SIG);
            put_u64(&mut self.out, 44); // size of the rest of THIS record
            put_u16(&mut self.out, VERSION_ZIP64);
            put_u16(&mut self.out, VERSION_ZIP64);
            put_u32(&mut self.out, 0);
            put_u32(&mut self.out, 0);
            put_u64(&mut self.out, n as u64);
            put_u64(&mut self.out, n as u64);
            put_u64(&mut self.out, cd_size);
            put_u64(&mut self.out, cd_offset);
            put_u32(&mut self.out, EOCD64_LOC_SIG);
            put_u32(&mut self.out, 0);
            put_u64(&mut self.out, eocd64);
            put_u32(&mut self.out, 1);
        }
        put_u32(&mut self.out, EOCD_SIG);
        put_u16(&mut self.out, 0);
        put_u16(&mut self.out, 0);
        let n16 = u16::try_from(n).unwrap_or(u16::MAX);
        put_u16(&mut self.out, n16);
        put_u16(&mut self.out, n16);
        put_u32(&mut self.out, u32::try_from(cd_size).unwrap_or(u32::MAX));
        put_u32(&mut self.out, u32::try_from(cd_offset).unwrap_or(u32::MAX));
        put_u16(&mut self.out, 0);
        Ok(())
    }

    /// A central directory entry, with its zip64 extra if needed.
    ///
    /// **If it's needed for ONE, all THREE fixed fields go to the
    /// sentinel.** The 0x0001 extra carries only the fields that read
    /// `0xFFFFFFFF` in the fixed record, in order (APPNOTE 4.5.3), so
    /// emitting all three values while marking only one makes a
    /// conforming reader — ours included, `zip_cd::resolve_extra`, which
    /// is strict on purpose — read the extra's first u64 as if it were
    /// the field that WAS marked. An archive over 4 GiB with small
    /// members took the SIZE as the local header's offset, and nobody
    /// could open it.
    fn write_cd(&mut self, e: &Written) {
        let zip64 = e.comp_size > U32_MAX || e.uncomp_size > U32_MAX || e.offset > U32_MAX;
        let mut extra: Vec<u8> = Vec::new();
        if zip64 {
            put_u16(&mut extra, 0x0001);
            put_u16(&mut extra, 24);
            put_u64(&mut extra, e.uncomp_size);
            put_u64(&mut extra, e.comp_size);
            put_u64(&mut extra, e.offset);
        }
        // The sentinel, for all three at once.
        let fixed = |v: u64| if zip64 { u32::MAX } else { truncate(v) };
        put_u32(&mut self.out, CD_SIG);
        // "Made by": 3 = Unix in the high byte, so the external attrs
        // field's permissions mean something.
        put_u16(&mut self.out, (3 << 8) | VERSION_BASE);
        put_u16(
            &mut self.out,
            if zip64 { VERSION_ZIP64 } else { VERSION_BASE },
        );
        put_u16(&mut self.out, e.flags);
        put_u16(&mut self.out, e.method);
        put_u16(&mut self.out, e.dos_time);
        put_u16(&mut self.out, e.dos_date);
        put_u32(&mut self.out, e.crc);
        put_u32(&mut self.out, fixed(e.comp_size));
        put_u32(&mut self.out, fixed(e.uncomp_size));
        put_u16(
            &mut self.out,
            u16::try_from(e.name.len()).unwrap_or(u16::MAX),
        );
        put_u16(&mut self.out, u16::try_from(extra.len()).unwrap_or(0));
        // Comment, starting disk, internal attributes: all three zero.
        // After that come the EXTERNAL ones (4) and the local header's
        // offset (4), and nothing else — a spare field here shifts the
        // name and the directory stops parsing four bytes further along.
        put_u16(&mut self.out, 0);
        put_u16(&mut self.out, 0);
        put_u16(&mut self.out, 0);
        put_u32(&mut self.out, e.external_attrs);
        put_u32(&mut self.out, fixed(e.offset));
        self.out.extend_from_slice(&e.name);
        self.out.extend_from_slice(&extra);
    }
}

/// `0xFFFFFFFF` is the sentinel that says "look at the zip64 extra".
fn truncate(v: u64) -> u32 {
    u32::try_from(v).unwrap_or(u32::MAX)
}

/// Unix permissions in the high byte, plus MS-DOS's directory bit.
fn external_attrs(entry: &PackEntry) -> u32 {
    let mode = entry.mode.unwrap_or(if entry.dir { 0o755 } else { 0o644 });
    let kind = if entry.dir { 0o040_000 } else { 0o100_000 };
    ((kind | (mode & 0o7777)) << 16) | u32::from(entry.dir)
}

/// Date and time in MS-DOS format, which is what zip carries.
///
/// It doesn't exist in that format before 1980: it's pinned to January 1,
/// 1980, which is what everybody does. `None` is the same — a zero there
/// would be an invalid date, not an absent one.
fn dos_datetime(mtime_ms: Option<i64>) -> (u16, u16) {
    const EPOCH_DOS: (u16, u16) = (0, 0b0000_0000_0010_0001);
    let Some(ms) = mtime_ms else {
        return EPOCH_DOS;
    };
    let secs = ms.div_euclid(1000);
    let Some(dt) = civil_from_unix(secs) else {
        return EPOCH_DOS;
    };
    if dt.year < 1980 {
        return EPOCH_DOS;
    }
    let year = u16::try_from(dt.year - 1980).unwrap_or(0);
    let date = (year << 9) | (u16::from(dt.month) << 5) | u16::from(dt.day);
    let time = (u16::from(dt.hour) << 11) | (u16::from(dt.min) << 5) | u16::from(dt.sec / 2);
    (time, date)
}

/// UTC civil date from seconds since the epoch.
pub(super) struct Civil {
    pub(super) year: i64,
    pub(super) month: u8,
    pub(super) day: u8,
    pub(super) hour: u8,
    pub(super) min: u8,
    pub(super) sec: u8,
}

/// Howard Hinnant's algorithm (`civil_from_days`), with no dependencies:
/// the alternative was dragging `chrono` into a provider crate for a date
/// that's only ever written.
pub(super) fn civil_from_unix(secs: i64) -> Option<Civil> {
    let days = secs.div_euclid(86_400);
    let remainder = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    Some(Civil {
        year: if m <= 2 { y + 1 } else { y },
        month: u8::try_from(m).ok()?,
        day: u8::try_from(d).ok()?,
        hour: u8::try_from(remainder / 3600).ok()?,
        min: u8::try_from((remainder % 3600) / 60).ok()?,
        sec: u8::try_from(remainder % 60).ok()?,
    })
}

fn put_u16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn put_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn put_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An entry's offset beyond 4 GiB travels through the zip64 extra,
    /// and all THREE fixed fields go to the sentinel.
    ///
    /// With only the offset marked, a conforming reader — ours among them
    /// — reads the extra's first u64 (the UNcompressed size) as if it
    /// were the offset, jumps there, doesn't find the local header's
    /// signature and returns `Corrupt`. A zip over 4 GiB with small
    /// members is about the most common thing there is, and nobody could
    /// open it.
    #[test]
    fn an_entry_beyond_4gib_marks_all_three_fields() {
        const HIGH: u64 = 0x1_0000_0000;
        let mut w = ZipWriter::desde(HIGH, 0);
        w.begin(&PackEntry::file(b"x".to_vec(), 4)).expect("opens");
        w.data(b"data").expect("data");
        w.end().expect("closes");
        w.finish().expect("finishes");
        let bytes = w.take();

        // The central directory entry starts at its signature.
        let cd = bytes
            .windows(4)
            .position(|v| v == CD_SIG.to_le_bytes())
            .expect("there is a central directory");
        let le32 = |i: usize| {
            u32::from_le_bytes([
                bytes[cd + i],
                bytes[cd + i + 1],
                bytes[cd + i + 2],
                bytes[cd + i + 3],
            ])
        };
        assert_eq!(le32(20), u32::MAX, "compressed to the sentinel");
        assert_eq!(le32(24), u32::MAX, "uncompressed too");
        assert_eq!(
            le32(42),
            u32::MAX,
            "and the offset, which is the one passed in"
        );

        let extra_len = u16::from_le_bytes([bytes[cd + 30], bytes[cd + 31]]);
        assert_eq!(extra_len, 28, "4-byte header + three u64s");
        let name_len = u16::from_le_bytes([bytes[cd + 28], bytes[cd + 29]]);
        let extra = cd + 46 + usize::from(name_len);
        let le64 = |i: usize| {
            let mut v = [0_u8; 8];
            v.copy_from_slice(&bytes[i..i + 8]);
            u64::from_le_bytes(v)
        };
        assert_eq!(le64(extra + 20), HIGH, "the REAL offset, the third one");
    }

    /// And an entry declaring more than 4 GiB says so in its LOCAL
    /// header: it's the only thing a streaming reader has to know the
    /// data descriptor carries eight bytes per size and not four.
    #[test]
    fn a_large_entry_announces_it_in_the_local_header() {
        let mut w = ZipWriter::new(0);
        // It's DECLARED large and not written: what's tested is the header.
        w.begin(&PackEntry::file(b"g".to_vec(), U32_MAX + 1))
            .expect("opens");
        let bytes = w.take();
        assert_eq!(
            u16::from_le_bytes([bytes[4], bytes[5]]),
            VERSION_ZIP64,
            "required version 4.5"
        );
        let extra_len = u16::from_le_bytes([bytes[28], bytes[29]]);
        assert_eq!(
            extra_len, 20,
            "and the 16-byte 0x0001 extra with its header"
        );
    }

    /// A normal entry carries NONE of that: an ordinary zip has to stay
    /// an ordinary zip.
    #[test]
    fn a_normal_entry_does_not_announce_zip64() {
        let mut w = ZipWriter::new(6);
        w.begin(&PackEntry::file(b"p".to_vec(), 4)).expect("opens");
        let bytes = w.take();
        assert_eq!(u16::from_le_bytes([bytes[4], bytes[5]]), VERSION_BASE);
        assert_eq!(u16::from_le_bytes([bytes[28], bytes[29]]), 0, "no extra");
    }
}
