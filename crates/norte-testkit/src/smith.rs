//! Deterministic forges of hostile compressed archives (phase 8, ADR 0018).
//!
//! The zip/tar fixtures are CODE, not committed binaries: byte-for-byte
//! control (raw cp437 names, a lying bit 11, zip-slip, a fake EOCD) with no
//! `.gitattributes` or regenerators. The forge does not compress: `stored`
//! by default; a real deflate entry goes through
//! [`ZipSmith::file_deflate`] with bytes ALREADY compressed by the caller
//! (#59 — decompression is exercised by the provider's own tests).

/// Bit-by-bit CRC-32 (IEEE, reflected) — enough for fixtures.
fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = !0;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// Fixed DOS date/time (2020-01-01 00:00:00): total determinism.
const DOS_TIME: u16 = 0;
const DOS_DATE: u16 = ((2020 - 1980) << 9) | (1 << 5) | 1;

enum ZipEntry {
    /// `utf8_flag` = bit 11 of the general purpose flag (name declares UTF-8).
    File {
        name: Vec<u8>,
        data: Vec<u8>,
        utf8_flag: bool,
    },
    /// Explicit directory entry (name with a trailing `/`).
    Dir { name: Vec<u8> },
    /// Entry with RAW method/flags (simulated encryption bit 0, exotic
    /// methods): `data` goes as-is — fixtures are not decompressed.
    Raw {
        name: Vec<u8>,
        data: Vec<u8>,
        method: u16,
        flags: u16,
    },
    /// A `stored` entry with a RAW extra field in the CENTRAL directory
    /// (#59: 0x7075 Info-ZIP, a forged zip64, arbitrary garbage). The local
    /// header is left WITHOUT extra: what is being tested lives in the CD.
    WithExtra {
        name: Vec<u8>,
        data: Vec<u8>,
        extra: Vec<u8>,
    },
    /// A REAL deflate entry (#59): `deflated` is the bytes ALREADY
    /// compressed (the caller produces them, e.g. with flate2 — this forge
    /// does not compress); the crc and uncompressed size are computed from
    /// `uncomp`.
    Deflate {
        name: Vec<u8>,
        uncomp: Vec<u8>,
        deflated: Vec<u8>,
    },
}

/// What ending the forged zip carries: a classic EOCD or a zip64 chain (#59).
enum ZipEnd {
    /// A 22-byte EOCD; `claimed` forces a lying count.
    Classic { claimed: Option<u16> },
    /// EOCD64 + locator + EOCD with markers (`0xFFFF`/`0xFFFF_FFFF`): the
    /// real count/size/offset live ONLY in the EOCD64. `claimed` forces a
    /// lying 64-bit count in the EOCD64.
    Zip64 { claimed: Option<u64> },
}

/// Entry-by-entry ZIP byte forge. See the module doc for why.
///
/// ```
/// let bytes = norte_testkit::ZipSmith::new()
///     .file(b"docs/hola.txt", b"hola")
///     .dir(b"empty")
///     .build();
/// assert_eq!(&bytes[..4], b"PK\x03\x04");
/// ```
#[derive(Default)]
pub struct ZipSmith {
    entries: Vec<ZipEntry>,
    comment: Vec<u8>,
    /// #100.2: the LAST entry in the CD declares this `comment_len` without
    /// writing its bytes — a CD truncated mid per-entry comment.
    cd_comment_len_lie: Option<u16>,
    /// The DOS date for ALL entries. [`ZipSmith::undated`] sets it to zero,
    /// which is an INVALID pair (month 0, day 0) and not a 1980 date.
    dos_date: Option<u16>,
}

impl ZipSmith {
    /// Empty forge.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A zip whose entries carry NO usable date: the DOS pair comes out
    /// zero, which is invalid (month 0, day 0) and which an honest reader
    /// has to report as "no date", never as 1980-00-00.
    ///
    /// It is the case of a zip written by a tool that leaves the field
    /// blank, and the only one through which a comparison against an
    /// archive can reach `CompareConfidence::Unknown` via the date.
    ///
    /// ```
    /// let bytes = norte_testkit::ZipSmith::new().undated().file(b"a", b"x").build();
    /// // Local header offset 10: dos_time (u16) followed by dos_date (u16).
    /// assert_eq!(&bytes[10..14], &[0, 0, 0, 0]);
    /// ```
    #[must_use]
    pub fn undated(mut self) -> Self {
        self.dos_date = Some(0);
        self
    }

    /// A `stored` file with bit 11 OFF (name in raw bytes: cp437, Latin-1,
    /// whatever — the historical case).
    #[must_use]
    pub fn file(mut self, name: &[u8], data: &[u8]) -> Self {
        self.entries.push(ZipEntry::File {
            name: name.to_vec(),
            data: data.to_vec(),
            utf8_flag: false,
        });
        self
    }

    /// A `stored` file with bit 11 ON (the name DECLARES UTF-8 — nobody
    /// verifies it is true: this also forges lying flags).
    #[must_use]
    pub fn file_utf8(mut self, name: &[u8], data: &[u8]) -> Self {
        self.entries.push(ZipEntry::File {
            name: name.to_vec(),
            data: data.to_vec(),
            utf8_flag: true,
        });
        self
    }

    /// Explicit directory entry; adds the trailing `/` if missing.
    #[must_use]
    pub fn dir(mut self, name: &[u8]) -> Self {
        let mut name = name.to_vec();
        if name.last() != Some(&b'/') {
            name.push(b'/');
        }
        self.entries.push(ZipEntry::Dir { name });
        self
    }

    /// An entry with raw `method`/`flags`: simulated encryption (bit 0),
    /// unsupported methods (99 = AES), whatever needs breaking.
    #[must_use]
    pub fn file_raw(mut self, name: &[u8], data: &[u8], method: u16, flags: u16) -> Self {
        self.entries.push(ZipEntry::Raw {
            name: name.to_vec(),
            data: data.to_vec(),
            method,
            flags,
        });
        self
    }

    /// A `stored` file with a RAW extra field in its CENTRAL directory entry
    /// (#59): 0x7075 Info-ZIP unicode path, a forged zip64 or arbitrary
    /// garbage. The local header is left WITHOUT extra.
    #[must_use]
    pub fn file_with_extra(mut self, name: &[u8], data: &[u8], extra: &[u8]) -> Self {
        self.entries.push(ZipEntry::WithExtra {
            name: name.to_vec(),
            data: data.to_vec(),
            extra: extra.to_vec(),
        });
        self
    }

    /// A REAL `deflate` file (#59): `deflated` is the bytes ALREADY
    /// compressed (the caller produces them with flate2 — this forge does
    /// not compress); the crc and uncompressed size come from `uncomp`.
    #[must_use]
    pub fn file_deflate(mut self, name: &[u8], uncomp: &[u8], deflated: &[u8]) -> Self {
        self.entries.push(ZipEntry::Deflate {
            name: name.to_vec(),
            uncomp: uncomp.to_vec(),
            deflated: deflated.to_vec(),
        });
        self
    }

    /// The EOCD's comment (arbitrary bytes — even fake EOCD signatures, the
    /// classic that breaks naive locators).
    #[must_use]
    pub fn comment(mut self, bytes: &[u8]) -> Self {
        self.comment = bytes.to_vec();
        self
    }

    /// The LAST entry in the CD declares `len` bytes of per-entry comment
    /// WITHOUT writing them: the EOCD's `cd_size` does not cover them, so
    /// the CD walk falls short mid-comment (#100.2 — pins
    /// `skipped != comment_len → Corrupt`). A no-op with no entries.
    #[must_use]
    pub fn cd_comment_len_lie(mut self, len: u16) -> Self {
        self.cd_comment_len_lie = Some(len);
        self
    }

    /// The full ZIP's bytes (local headers + central directory + EOCD).
    #[must_use]
    pub fn build(self) -> Vec<u8> {
        self.build_end(&ZipEnd::Classic { claimed: None })
    }

    /// Like [`Self::build`] but the EOCD LIES: it declares `claimed` entries
    /// (a cheap index bomb: announces millions without paying for them).
    #[must_use]
    pub fn build_lying_eocd(self, claimed: u16) -> Vec<u8> {
        self.build_end(&ZipEnd::Classic {
            claimed: Some(claimed),
        })
    }

    /// Like [`Self::build`] but with a zip64 ending (#59): EOCD64 + locator +
    /// EOCD with markers (`0xFFFF`/`0xFFFF_FFFF`) — the real
    /// count/size/offset live ONLY in the EOCD64.
    #[must_use]
    pub fn build_zip64(self) -> Vec<u8> {
        self.build_end(&ZipEnd::Zip64 { claimed: None })
    }

    /// Like [`Self::build_zip64`] but the EOCD64 LIES about the count (a
    /// zip64 index bomb: the classic u16 preflight would not see it, #59).
    #[must_use]
    pub fn build_zip64_lying_count(self, claimed: u64) -> Vec<u8> {
        self.build_end(&ZipEnd::Zip64 {
            claimed: Some(claimed),
        })
    }

    #[expect(
        clippy::cast_possible_truncation,
        reason = "fixtures are small by design"
    )]
    fn build_end(self, end: &ZipEnd) -> Vec<u8> {
        let mut out = Vec::new();
        let mut central = Vec::new();
        let real_count = self.entries.len() as u16;
        let last_idx = self.entries.len().wrapping_sub(1);
        let dos_date = self.dos_date.unwrap_or(DOS_DATE);
        for (idx, entry) in self.entries.iter().enumerate() {
            let ZipWire {
                name,
                payload,
                flags,
                method,
                crc,
                uncomp_len,
                extra,
            } = entry.wire();
            let offset = out.len() as u32;
            let comp_len = payload.len() as u32;
            // Local file header.
            out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
            out.extend_from_slice(&20u16.to_le_bytes()); // version needed
            out.extend_from_slice(&flags.to_le_bytes());
            out.extend_from_slice(&method.to_le_bytes());
            out.extend_from_slice(&DOS_TIME.to_le_bytes());
            out.extend_from_slice(&dos_date.to_le_bytes());
            out.extend_from_slice(&crc.to_le_bytes());
            out.extend_from_slice(&comp_len.to_le_bytes()); // compressed
            out.extend_from_slice(&uncomp_len.to_le_bytes()); // uncompressed
            out.extend_from_slice(&(name.len() as u16).to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes()); // extra
            out.extend_from_slice(name);
            out.extend_from_slice(payload);
            // Central directory entry.
            central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
            central.extend_from_slice(&20u16.to_le_bytes()); // made by
            central.extend_from_slice(&20u16.to_le_bytes()); // needed
            central.extend_from_slice(&flags.to_le_bytes());
            central.extend_from_slice(&method.to_le_bytes());
            central.extend_from_slice(&DOS_TIME.to_le_bytes());
            central.extend_from_slice(&dos_date.to_le_bytes());
            central.extend_from_slice(&crc.to_le_bytes());
            central.extend_from_slice(&comp_len.to_le_bytes());
            central.extend_from_slice(&uncomp_len.to_le_bytes());
            central.extend_from_slice(&(name.len() as u16).to_le_bytes());
            central.extend_from_slice(&(extra.len() as u16).to_le_bytes());
            // Per-entry comment: 0 except for #100.2's lie on the last
            // entry (declares bytes that are NOT written to the CD).
            let entry_comment_len = if idx == last_idx {
                self.cd_comment_len_lie.unwrap_or(0)
            } else {
                0
            };
            central.extend_from_slice(&entry_comment_len.to_le_bytes()); // comment
            central.extend_from_slice(&0u16.to_le_bytes()); // disk
            central.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
            let external: u32 = match entry {
                ZipEntry::Dir { .. } => 0x10, // DOS directory bit
                _ => 0,
            };
            central.extend_from_slice(&external.to_le_bytes());
            central.extend_from_slice(&offset.to_le_bytes());
            central.extend_from_slice(name);
            central.extend_from_slice(extra);
        }
        let cd_offset = out.len() as u64;
        let cd_size = central.len() as u64;
        out.extend_from_slice(&central);
        emit_zip_end(&mut out, end, real_count, cd_offset, cd_size);
        out.extend_from_slice(&(self.comment.len() as u16).to_le_bytes());
        out.extend_from_slice(&self.comment);
        out
    }
}

/// The already-resolved fields an entry contributes to the local header and
/// the CD.
struct ZipWire<'a> {
    name: &'a [u8],
    /// Bytes written after the local header (compressed if `Deflate`).
    payload: &'a [u8],
    flags: u16,
    method: u16,
    /// Declared CRC (of the UNCOMPRESSED content).
    crc: u32,
    /// Declared uncompressed size.
    uncomp_len: u32,
    /// The CD's extra field (the local one ALWAYS goes without extra).
    extra: &'a [u8],
}

impl ZipEntry {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "fixtures are small by design"
    )]
    fn wire(&self) -> ZipWire<'_> {
        match self {
            ZipEntry::File {
                name,
                data,
                utf8_flag,
            } => ZipWire {
                name,
                payload: data,
                flags: if *utf8_flag { 1u16 << 11 } else { 0 },
                method: 0,
                crc: crc32(data),
                uncomp_len: data.len() as u32,
                extra: &[],
            },
            ZipEntry::Dir { name } => ZipWire {
                name,
                payload: &[],
                flags: 0,
                method: 0,
                crc: crc32(&[]),
                uncomp_len: 0,
                extra: &[],
            },
            ZipEntry::Raw {
                name,
                data,
                method,
                flags,
            } => ZipWire {
                name,
                payload: data,
                flags: *flags,
                method: *method,
                crc: crc32(data),
                uncomp_len: data.len() as u32,
                extra: &[],
            },
            ZipEntry::WithExtra { name, data, extra } => ZipWire {
                name,
                payload: data,
                flags: 0,
                method: 0,
                crc: crc32(data),
                uncomp_len: data.len() as u32,
                extra,
            },
            ZipEntry::Deflate {
                name,
                uncomp,
                deflated,
            } => ZipWire {
                name,
                payload: deflated,
                flags: 0,
                method: 8,
                crc: crc32(uncomp),
                uncomp_len: uncomp.len() as u32,
                extra: &[],
            },
        }
    }
}

/// Emits the zip's ending: a classic EOCD or an EOCD64 + locator + EOCD
/// chain with markers (#59). The comment is written by the caller afterward.
#[expect(
    clippy::cast_possible_truncation,
    reason = "fixtures are small by design"
)]
fn emit_zip_end(out: &mut Vec<u8>, end: &ZipEnd, real_count: u16, cd_offset: u64, cd_size: u64) {
    match end {
        ZipEnd::Classic { claimed } => {
            let count = claimed.unwrap_or(real_count);
            out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes()); // disk
            out.extend_from_slice(&0u16.to_le_bytes()); // CD's disk
            out.extend_from_slice(&count.to_le_bytes());
            out.extend_from_slice(&count.to_le_bytes());
            out.extend_from_slice(&(cd_size as u32).to_le_bytes());
            out.extend_from_slice(&(cd_offset as u32).to_le_bytes());
        }
        ZipEnd::Zip64 { claimed } => {
            let count = claimed.unwrap_or(u64::from(real_count));
            let eocd64_pos = out.len() as u64;
            // EOCD64 (56 bytes: record size = 44, what follows the first
            // 12).
            out.extend_from_slice(&0x0606_4b50u32.to_le_bytes());
            out.extend_from_slice(&44u64.to_le_bytes()); // size of record
            out.extend_from_slice(&45u16.to_le_bytes()); // made by
            out.extend_from_slice(&45u16.to_le_bytes()); // needed
            out.extend_from_slice(&0u32.to_le_bytes()); // disk
            out.extend_from_slice(&0u32.to_le_bytes()); // CD's disk
            out.extend_from_slice(&count.to_le_bytes()); // on this disk
            out.extend_from_slice(&count.to_le_bytes()); // total
            out.extend_from_slice(&cd_size.to_le_bytes());
            out.extend_from_slice(&cd_offset.to_le_bytes());
            // EOCD64 locator (20 bytes, RIGHT before the EOCD).
            out.extend_from_slice(&0x0706_4b50u32.to_le_bytes());
            out.extend_from_slice(&0u32.to_le_bytes()); // EOCD64's disk
            out.extend_from_slice(&eocd64_pos.to_le_bytes());
            out.extend_from_slice(&1u32.to_le_bytes()); // total disks
            // Classic EOCD with MARKERS: the real values live in the
            // EOCD64.
            out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes()); // disk
            out.extend_from_slice(&0u16.to_le_bytes()); // CD's disk
            out.extend_from_slice(&u16::MAX.to_le_bytes());
            out.extend_from_slice(&u16::MAX.to_le_bytes());
            out.extend_from_slice(&u32::MAX.to_le_bytes());
            out.extend_from_slice(&u32::MAX.to_le_bytes());
        }
    }
}

enum TarEntry {
    File {
        name: Vec<u8>,
        data: Vec<u8>,
    },
    Dir {
        name: Vec<u8>,
    },
    Symlink {
        name: Vec<u8>,
        target: Vec<u8>,
    },
    /// A file with a LONG name via GNU longname (#60): an `L` entry
    /// (`"././@LongLink"`, data = the real name + NUL) followed by the file
    /// with its name TRUNCATED to 100 in its header.
    GnuLongName {
        name: Vec<u8>,
        data: Vec<u8>,
    },
    /// A file with a pax `path=` override (#60): an `x` entry with the pax
    /// record followed by the file with a placeholder name.
    PaxPath {
        name: Vec<u8>,
        data: Vec<u8>,
    },
    /// A RAW entry with an arbitrary typeflag (#60): metadata pins the `tar`
    /// crate's iterator consumes or must filter (`g` =
    /// `pax_global_header`, H5).
    Raw {
        typeflag: u8,
        name: Vec<u8>,
        data: Vec<u8>,
    },
}

/// Tar byte forge (plain ustar + GNU longname and pax `path=` since #60). A
/// ustar header only holds 100 bytes of name: long ones go through
/// [`TarSmith::file_gnu_longname`] / [`TarSmith::file_pax_path`]; `file`
/// with a name >100 still PANICS (explicit forge contract).
///
/// ```
/// let bytes = norte_testkit::TarSmith::new()
///     .file(b"docs/hola.txt", b"hola")
///     .build();
/// assert_eq!(&bytes[257..262], b"ustar");
/// ```
#[derive(Default)]
pub struct TarSmith {
    entries: Vec<TarEntry>,
}

impl TarSmith {
    /// Empty forge.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Regular file.
    #[must_use]
    pub fn file(mut self, name: &[u8], data: &[u8]) -> Self {
        self.entries.push(TarEntry::File {
            name: name.to_vec(),
            data: data.to_vec(),
        });
        self
    }

    /// Explicit directory; adds the trailing `/` if missing.
    #[must_use]
    pub fn dir(mut self, name: &[u8]) -> Self {
        let mut name = name.to_vec();
        if name.last() != Some(&b'/') {
            name.push(b'/');
        }
        self.entries.push(TarEntry::Dir { name });
        self
    }

    /// Symlink with a raw-bytes target.
    #[must_use]
    pub fn symlink(mut self, name: &[u8], target: &[u8]) -> Self {
        self.entries.push(TarEntry::Symlink {
            name: name.to_vec(),
            target: target.to_vec(),
        });
        self
    }

    /// A file with a name of ANY length via GNU longname (#60, H6): an `L`
    /// entry with the real name as data + the file with its name truncated
    /// to 100 in its ustar header — like real GNU tar.
    #[must_use]
    pub fn file_gnu_longname(mut self, name: &[u8], data: &[u8]) -> Self {
        self.entries.push(TarEntry::GnuLongName {
            name: name.to_vec(),
            data: data.to_vec(),
        });
        self
    }

    /// A file with a pax `path=` override (#60, H6): an `x` entry with the
    /// `LEN path=NAME\n` record (raw bytes — real pax requires UTF-8,
    /// hostile tars do not) + the file with a placeholder name.
    #[must_use]
    pub fn file_pax_path(mut self, name: &[u8], data: &[u8]) -> Self {
        self.entries.push(TarEntry::PaxPath {
            name: name.to_vec(),
            data: data.to_vec(),
        });
        self
    }

    /// A raw entry with an arbitrary `typeflag` (#60): e.g. `b'g'` to pin
    /// `pax_global_header`'s filter (H5).
    #[must_use]
    pub fn entry_raw(mut self, typeflag: u8, name: &[u8], data: &[u8]) -> Self {
        self.entries.push(TarEntry::Raw {
            typeflag,
            name: name.to_vec(),
            data: data.to_vec(),
        });
        self
    }

    /// The full tar's bytes (512-byte headers + data + 2 zero blocks).
    ///
    /// # Panics
    /// Name or target > 100 bytes (see the type's doc).
    #[must_use]
    pub fn build(self) -> Vec<u8> {
        let mut out = Vec::new();
        for entry in &self.entries {
            match entry {
                TarEntry::File { name, data } => emit_tar(&mut out, name, data, b'0', &[]),
                TarEntry::Dir { name } => emit_tar(&mut out, name, &[], b'5', &[]),
                TarEntry::Symlink { name, target } => emit_tar(&mut out, name, &[], b'2', target),
                TarEntry::GnuLongName { name, data } => {
                    // GNU longname: an `L` entry with the real name + NUL as
                    // data; the file's header carries the truncated name.
                    let mut long = name.clone();
                    long.push(0);
                    emit_tar(&mut out, b"././@LongLink", &long, b'L', &[]);
                    emit_tar(&mut out, &name[..name.len().min(100)], data, b'0', &[]);
                }
                TarEntry::PaxPath { name, data } => {
                    // Pax record `LEN path=NAME\n` with LEN = the record's
                    // TOTAL length (digits included) — the format's classic
                    // iterative calculation.
                    let base = " path=".len() + name.len() + 1;
                    let mut len = base + 1;
                    while len.to_string().len() + base != len {
                        len = len.to_string().len() + base;
                    }
                    let mut record = format!("{len} path=").into_bytes();
                    record.extend_from_slice(name);
                    record.push(b'\n');
                    emit_tar(&mut out, b"PaxHeader/x", &record, b'x', &[]);
                    emit_tar(&mut out, b"placeholder", data, b'0', &[]);
                }
                TarEntry::Raw {
                    typeflag,
                    name,
                    data,
                } => emit_tar(&mut out, name, data, *typeflag, &[]),
            }
        }
        out.extend(std::iter::repeat_n(0u8, 1024));
        out
    }
}

/// Emits ONE 512-byte ustar header + data + padding. `name`/`link` ≤ 100
/// bytes (panic: forge contract, see [`TarSmith`]'s doc).
fn emit_tar(out: &mut Vec<u8>, name: &[u8], data: &[u8], typeflag: u8, link: &[u8]) {
    assert!(
        name.len() <= 100 && link.len() <= 100,
        "TarSmith does not forge headers with a name >100 bytes (use file_gnu_longname/file_pax_path)"
    );
    let mut header = [0u8; 512];
    header[..name.len()].copy_from_slice(name);
    header[100..107].copy_from_slice(b"0000644"); // mode
    header[108..115].copy_from_slice(b"0000000"); // uid
    header[116..123].copy_from_slice(b"0000000"); // gid
    let size_field = format!("{:011o}", data.len());
    header[124..135].copy_from_slice(size_field.as_bytes());
    header[136..147].copy_from_slice(b"00000000000"); // mtime 1970
    header[148..156].copy_from_slice(b"        "); // blank checksum
    header[156] = typeflag;
    header[157..157 + link.len()].copy_from_slice(link);
    // POSIX magic ("ustar\0" + "00") also on the L entry: real GNU tar
    // emits the old-GNU magic ("ustar  \0") — tar-rs honors L with either
    // (audit #60 nit; a parser that demanded the GNU magic would diverge).
    header[257..262].copy_from_slice(b"ustar");
    header[263..265].copy_from_slice(b"00");
    let sum: u32 = header.iter().map(|&b| u32::from(b)).sum();
    let chk = format!("{sum:06o}\0 ");
    header[148..156].copy_from_slice(chk.as_bytes());
    out.extend_from_slice(&header);
    out.extend_from_slice(data);
    let rem = data.len() % 512;
    if rem != 0 {
        out.extend(std::iter::repeat_n(0u8, 512 - rem));
    }
}

// --- RAR5 -----------------------------------------------------------------

/// RAR5's variable-length integer: 7 bits per byte, high bit = "continues".
fn vint(mut n: u64) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let b = u8::try_from(n & 0x7f).expect("7 bits fit in a u8");
        n >>= 7;
        if n == 0 {
            out.push(b);
            return out;
        }
        out.push(b | 0x80);
    }
}

/// The `7z` executable if it is on `PATH`. Tests that need a real delegate
/// bow out saying so when it returns `None`.
///
/// ```
/// // On a machine without 7z installed this is `None`, and that is not a failure.
/// let _ = norte_testkit::which_7z();
/// ```
#[must_use]
pub fn which_7z() -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        for exe in ["7z", "7zz"] {
            let candidate = dir.join(exe);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// An entry of the RAR5 forge.
struct RarEntry {
    name: Vec<u8>,
    content: Vec<u8>,
    is_dir: bool,
}

/// RAR5 byte forge with STORED entries (method 0), sibling of [`ZipSmith`]
/// and [`TarSmith`].
///
/// It exists because RAR's compressor is the non-free half: nothing in this
/// tree can produce a compressed `.rar`, so without this there is no
/// fixture at all — no hostile corpus, no contractual suite. The CONTAINER
/// is documented and putting raw bytes inside it does not touch the
/// proprietary algorithm.
///
/// ```
/// let bytes = norte_testkit::RarSmith::new()
///     .file(b"docs/hola.txt", b"hola")
///     .build();
/// assert_eq!(&bytes[..8], b"Rar!\x1a\x07\x01\x00");
/// ```
#[derive(Default)]
pub struct RarSmith {
    entries: Vec<RarEntry>,
    mtime: u32,
}

impl RarSmith {
    /// Empty forge. Fixed `mtime` (2021-01-14) for total determinism.
    #[must_use]
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            mtime: 0x6000_0000,
        }
    }

    /// Stored file. `name` goes in RAW BYTES: a non-UTF8 name is exactly the
    /// case that must be forgeable (hard rule 1).
    #[must_use]
    pub fn file(mut self, name: &[u8], content: &[u8]) -> Self {
        self.entries.push(RarEntry {
            name: name.to_vec(),
            content: content.to_vec(),
            is_dir: false,
        });
        self
    }

    /// Explicit directory entry (no data).
    #[must_use]
    pub fn dir(mut self, name: &[u8]) -> Self {
        self.entries.push(RarEntry {
            name: name.to_vec(),
            content: Vec::new(),
            is_dir: true,
        });
        self
    }

    /// The `.rar`'s bytes.
    #[must_use]
    pub fn build(self) -> Vec<u8> {
        let mut out = Vec::from(*b"Rar!\x1a\x07\x01\x00");
        // Main header: head_type 1, no flags, ArchiveFlags = 0.
        out.extend_from_slice(&rar_block(1, 0, &vint(0), &[]));
        for e in &self.entries {
            out.extend_from_slice(&self.rar_file_block(e));
        }
        // End of archive: head_type 5, EndFlags = 0.
        out.extend_from_slice(&rar_block(5, 0, &vint(0), &[]));
        out
    }

    /// File header (`head_type` 2) followed by its raw data.
    fn rar_file_block(&self, e: &RarEntry) -> Vec<u8> {
        // FileFlags: 0x0001 directory | 0x0002 mtime present | 0x0004 crc.
        let file_flags: u64 = u64::from(e.is_dir) | 0x0002 | 0x0004;
        let attrs: u64 = if e.is_dir { 0x10 } else { 0x20 };
        let mut body = vint(file_flags);
        body.extend_from_slice(&vint(e.content.len() as u64));
        body.extend_from_slice(&vint(attrs));
        body.extend_from_slice(&self.mtime.to_le_bytes());
        body.extend_from_slice(&crc32(&e.content).to_le_bytes());
        // CompressionInfo: version 0, method 0 (stored), dictionary 0.
        body.extend_from_slice(&vint(0));
        // HostOS: 1 = unix.
        body.extend_from_slice(&vint(1));
        body.extend_from_slice(&vint(e.name.len() as u64));
        body.extend_from_slice(&e.name);

        // head_flags 0x0002 = the block declares DataSize (the bytes that
        // follow it). A directory carries no data and does not declare it.
        let mut block = if e.is_dir {
            rar_block(2, 0, &body, &[])
        } else {
            rar_block(2, 0x0002, &body, &e.content)
        };
        block.extend_from_slice(&e.content);
        block
    }

    /// The bytes of a **RAR4**, the old format, with names in RAW BYTES
    /// (#223).
    ///
    /// RAR5 stores names in UTF-8 by format, so `build` cannot write the
    /// case that really exists out there: **an archive made on a machine
    /// with an OEM code page** (CP437, CP866, CP1251…), which is what a
    /// decade of downloads contains. RAR4 does allow it: without the
    /// `LHD_UNICODE` flag (0x0200) the name travels as-is, and that is what
    /// this forges.
    ///
    /// The issue assumed forging RAR4 "starts to look like reimplementing
    /// the format we deliberately do not implement", and so proposed
    /// putting a third-party binary in the repo. It is not needed: what
    /// gets forged here is the CONTAINER with a STORED entry, same as in
    /// RAR5 — the proprietary algorithm, the part norte does not and will
    /// not implement, is not touched. And it comes out better than a
    /// binary: it is deterministic, raises no license or provenance
    /// questions, and can carry any name from the hostile corpus.
    ///
    /// **Verified against `unrar` 7.23 and `7z`**, which is what makes it a
    /// fixture and not a guess. It also happens to answer the three
    /// questions the issue left open: `7z -slt` prints the RAW OEM bytes;
    /// `unrar` does NOT —it maps them to a private-use range (U+E0xx)
    /// preceded by U+FFFE—; and neither TRUNCATES the name, which was the
    /// measured bug for non-UTF8 RAR5.
    ///
    /// File entries only: a RAR4 with explicit directories adds nothing
    /// RAR5 does not already cover.
    ///
    /// ```
    /// // `папка.txt` in CP866, the classic Russian name from a DOS machine.
    /// let name = b"\xaf\xa0\xaf\xaa\xa0.txt";
    /// let bytes = norte_testkit::RarSmith::new()
    ///     .file(name, b"hola")
    ///     .build_rar4();
    /// assert_eq!(&bytes[..7], b"Rar!\x1a\x07\x00");
    /// // The name is INSIDE, byte for byte and without transcoding.
    /// assert!(bytes.windows(name.len()).any(|w| w == name));
    /// ```
    #[must_use]
    pub fn build_rar4(self) -> Vec<u8> {
        // RAR4's marker ends in 0x00; RAR5's, in 0x01 0x00. It is the first
        // thing any reader looks at to know what it is talking to.
        let mut out = Vec::from(*b"Rar!\x1a\x07\x00");
        out.extend_from_slice(&rar4_main_head());
        for e in self.entries.iter().filter(|e| !e.is_dir) {
            out.extend_from_slice(&rar4_file_head(&e.name, &e.content));
        }
        out
    }
}

/// A RAR4's main header (`HEAD_TYPE` 0x73), thirteen bytes.
fn rar4_main_head() -> Vec<u8> {
    let mut body = vec![0x73, 0x00, 0x00, 13, 0x00];
    body.extend_from_slice(&[0u8; 6]); // RESERVED1(2) + RESERVED2(4)
    rar4_with_crc(&body)
}

/// A RAR4 file header (`HEAD_TYPE` 0x74) followed by its data.
///
/// Method 0x30 = STORED, the only thing this tree can write. No
/// `LHD_UNICODE` (0x0200) on purpose: the name is whatever bytes are passed
/// in.
fn rar4_file_head(name: &[u8], data: &[u8]) -> Vec<u8> {
    let size = u16::try_from(32 + name.len()).unwrap_or(u16::MAX);
    let n = u32::try_from(data.len()).unwrap_or(u32::MAX);
    let mut body = vec![0x74];
    // LHD_LONG_BLOCK (0x8000): the block is followed by its data.
    body.extend_from_slice(&0x8000u16.to_le_bytes());
    body.extend_from_slice(&size.to_le_bytes());
    body.extend_from_slice(&n.to_le_bytes()); // PACK_SIZE
    body.extend_from_slice(&n.to_le_bytes()); // UNP_SIZE
    // HOST_OS 0x02 = Win32, which is where OEM code pages come from.
    body.push(0x02);
    body.extend_from_slice(&crc32(data).to_le_bytes());
    body.extend_from_slice(&0x5000_0000u32.to_le_bytes()); // FTIME, fixed
    body.push(20); // UNP_VER 2.0
    body.push(0x30); // METHOD: stored
    body.extend_from_slice(&u16::try_from(name.len()).unwrap_or(u16::MAX).to_le_bytes());
    body.extend_from_slice(&0x20u32.to_le_bytes()); // ATTR
    body.extend_from_slice(name);
    let mut block = rar4_with_crc(&body);
    block.extend_from_slice(data);
    block
}

/// Prepends RAR4's `HEAD_CRC`: the LOW TWO BYTES of the CRC32 of the
/// header, counting from `HEAD_TYPE`.
fn rar4_with_crc(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len() + 2);
    // The truncation IS the format, not an oversight: RAR4 stores two bytes
    // where there is a CRC32, and they are the low ones. `unrar` validates
    // exactly those.
    #[allow(clippy::cast_possible_truncation)]
    let low = crc32(body) as u16;
    out.extend_from_slice(&low.to_le_bytes());
    out.extend_from_slice(body);
    out
}

/// A RAR5 block: `crc32(len ++ inner) ++ len ++ inner`, where `inner` is
/// `head_type ++ head_flags ++ [data_size] ++ body`. The CRC covers the
/// length and the interior, not the data that follows the block.
fn rar_block(head_type: u64, head_flags: u64, body: &[u8], data: &[u8]) -> Vec<u8> {
    let mut inner = vint(head_type);
    inner.extend_from_slice(&vint(head_flags));
    if head_flags & 0x0002 != 0 {
        inner.extend_from_slice(&vint(data.len() as u64));
    }
    inner.extend_from_slice(body);

    let mut hdr = vint(inner.len() as u64);
    hdr.extend_from_slice(&inner);

    let mut out = Vec::from(crc32(&hdr).to_le_bytes());
    out.extend_from_slice(&hdr);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_vectores_conocidos() {
        assert_eq!(crc32(b""), 0);
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn zip_estructura_coherente() {
        let z = ZipSmith::new()
            .file(b"a.txt", b"hola")
            .file_utf8("ñ.txt".as_bytes(), "eñe".as_bytes())
            .dir(b"sub")
            .build();
        assert_eq!(&z[..4], b"PK\x03\x04");
        // EOCD at the end, real count = 3.
        let eocd = &z[z.len() - 22..];
        assert_eq!(&eocd[..4], b"PK\x05\x06");
        assert_eq!(u16::from_le_bytes([eocd[10], eocd[11]]), 3);
        // The CD's offset points at a central directory signature.
        let cd_off = u32::from_le_bytes([eocd[16], eocd[17], eocd[18], eocd[19]]) as usize;
        assert_eq!(&z[cd_off..cd_off + 4], b"PK\x01\x02");
    }

    #[test]
    fn zip_eocd_mentiroso() {
        let z = ZipSmith::new().file(b"x", b"").build_lying_eocd(60_000);
        let eocd = &z[z.len() - 22..];
        assert_eq!(u16::from_le_bytes([eocd[10], eocd[11]]), 60_000);
    }

    #[test]
    fn zip64_estructura_coherente() {
        let z = ZipSmith::new().file(b"a", b"data").build_zip64();
        // Final EOCD with MARKERS.
        let eocd = &z[z.len() - 22..];
        assert_eq!(&eocd[..4], b"PK\x05\x06");
        assert_eq!(u16::from_le_bytes([eocd[10], eocd[11]]), u16::MAX);
        assert_eq!(
            u32::from_le_bytes([eocd[16], eocd[17], eocd[18], eocd[19]]),
            u32::MAX
        );
        // Locator 20 bytes earlier: points at an EOCD64 with the real count.
        let loc = &z[z.len() - 42..z.len() - 22];
        assert_eq!(&loc[..4], b"PK\x06\x07");
        let eocd64_pos =
            usize::try_from(u64::from_le_bytes(loc[8..16].try_into().unwrap())).unwrap();
        assert_eq!(&z[eocd64_pos..eocd64_pos + 4], b"PK\x06\x06");
        let count = u64::from_le_bytes(z[eocd64_pos + 32..eocd64_pos + 40].try_into().unwrap());
        assert_eq!(count, 1);
        // The lying count lives in the EOCD64, not in the EOCD.
        let liar = ZipSmith::new()
            .file(b"a", b"d")
            .build_zip64_lying_count(9_000_000);
        let loc = &liar[liar.len() - 42..liar.len() - 22];
        let pos = usize::try_from(u64::from_le_bytes(loc[8..16].try_into().unwrap())).unwrap();
        assert_eq!(
            u64::from_le_bytes(liar[pos + 32..pos + 40].try_into().unwrap()),
            9_000_000
        );
    }

    #[test]
    fn zip_extra_solo_en_el_cd() {
        let extra = [0x75u8, 0x70, 0x03, 0x00, 0x01, 0x02, 0x03]; // id 0x7075
        let z = ZipSmith::new().file_with_extra(b"n", b"d", &extra).build();
        // Local header: extra_len = 0 (the extra lives ONLY in the CD).
        assert_eq!(u16::from_le_bytes([z[28], z[29]]), 0);
        // CD: extra_len = 7 and the bytes are after the name.
        let cd = z.windows(4).position(|w| w == b"PK\x01\x02").expect("cd");
        assert_eq!(u16::from_le_bytes([z[cd + 30], z[cd + 31]]), 7);
        assert_eq!(&z[cd + 46 + 1..cd + 46 + 1 + 7], &extra);
    }

    #[test]
    fn zip_deflate_declares_real_sizes() {
        // Simulated "deflated" shorter than the content: comp != uncomp.
        let z = ZipSmith::new()
            .file_deflate(b"f", b"0123456789", b"XYZ")
            .build();
        let cd = z.windows(4).position(|w| w == b"PK\x01\x02").expect("cd");
        assert_eq!(u16::from_le_bytes([z[cd + 10], z[cd + 11]]), 8, "method");
        let comp = u32::from_le_bytes(z[cd + 20..cd + 24].try_into().unwrap());
        let uncomp = u32::from_le_bytes(z[cd + 24..cd + 28].try_into().unwrap());
        assert_eq!((comp, uncomp), (3, 10));
        let crc = u32::from_le_bytes(z[cd + 16..cd + 20].try_into().unwrap());
        assert_eq!(crc, crc32(b"0123456789"), "crc of the UNCOMPRESSED content");
    }

    #[test]
    fn tar_estructura_coherente() {
        let t = TarSmith::new()
            .file(b"docs/x.bin", &[0xFF; 700])
            .symlink(b"lnk", b"docs/x.bin")
            .build();
        // header + 700 padded to 1024 + symlink header + 1024 closing.
        assert_eq!(t.len(), 512 + 1024 + 512 + 1024);
        assert_eq!(&t[257..262], b"ustar");
        // checksum: recalculating with the field blanked out matches.
        let mut h = t[..512].to_vec();
        let stored: Vec<u8> = h[148..156].to_vec();
        h[148..156].copy_from_slice(b"        ");
        let sum: u32 = h.iter().map(|&b| u32::from(b)).sum();
        assert_eq!(format!("{sum:06o}\0 ").as_bytes(), stored.as_slice());
    }

    #[test]
    #[should_panic(expected = "does not forge headers with a name >100")]
    fn tar_long_name_panics() {
        let _ = TarSmith::new().file(&[b'a'; 101], b"").build();
    }

    /// #60 (audit INFO): the pax record's LEN at digit transitions — base 97
    /// → LEN 99 (2 digits), base 98 → 101 (skips the impossible 100), base
    /// 99 → 102. The emitted record measures EXACTLY its LEN.
    #[test]
    fn pax_len_in_digit_transitions() {
        for name_len in [91usize, 92, 93, 13] {
            let name = vec![b'n'; name_len];
            let tar = TarSmith::new().file_pax_path(&name, b"d").build();
            // The x entry is the first header: its data starts at 512.
            let size = usize::from_str_radix(
                std::str::from_utf8(&tar[124..135])
                    .unwrap()
                    .trim_end_matches('\0')
                    .trim(),
                8,
            )
            .expect("octal size");
            let record = &tar[512..512 + size];
            let space = record.iter().position(|&b| b == b' ').expect("LEN space");
            let len: usize = std::str::from_utf8(&record[..space])
                .unwrap()
                .parse()
                .expect("decimal LEN");
            assert_eq!(len, record.len(), "name_len={name_len}: LEN == real length");
        }
    }

    // --- RarSmith (ADR 0018 / roadmap item 11) -----------------------------

    /// The writer produces an archive the REAL DELEGATE knows how to read. A
    /// "correct" writer by our own reading proves nothing.
    #[test]
    fn a_real_delegate_lists_what_we_forged() {
        const RAW: &[u8] = b"cp437-\xa4\xa5.txt";
        let Some(sevenz) = which_7z() else {
            eprintln!("no 7z installed: test bowing out");
            return;
        };
        let bytes = RarSmith::new()
            .file(b"hello.txt", b"hola norte\n")
            .file("\u{f1}and\u{fa}.txt".as_bytes(), b"utf8\n")
            .file(RAW, b"bytes\n")
            .build();
        let dir = std::env::temp_dir().join(format!("norte-rarsmith-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("tempdir");
        let path = dir.join("t.rar");
        std::fs::write(&path, &bytes).expect("write");

        let out = std::process::Command::new(sevenz)
            .args(["l", "-slt", "-p", "--"])
            .arg(&path)
            .output()
            .expect("7z runs");
        std::fs::remove_dir_all(&dir).ok();
        assert!(out.status.success(), "7z failed: {out:?}");
        // The non-UTF8 name's RAW BYTES survive 7z's listing.
        assert!(
            out.stdout.windows(RAW.len()).any(|w| w == RAW),
            "the raw name does not appear in 7z's listing: {}",
            String::from_utf8_lossy(&out.stdout)
        );
    }

    #[test]
    fn vint_codifica_multibyte() {
        assert_eq!(vint(0), vec![0x00]);
        assert_eq!(vint(0x7f), vec![0x7f]);
        assert_eq!(vint(0x80), vec![0x80, 0x01]);
        assert_eq!(vint(0x3fff), vec![0xff, 0x7f]);
    }

    #[test]
    fn the_signature_is_rar5_and_the_content_goes_raw() {
        let bytes = RarSmith::new().file(b"a.txt", b"STORD").build();
        assert_eq!(&bytes[..8], b"Rar!\x1a\x07\x01\x00");
        // Method 0 = stored: the content is literally right there.
        assert!(
            bytes.windows(5).any(|w| w == b"STORD"),
            "a stored entry does not compress anything"
        );
    }
}
