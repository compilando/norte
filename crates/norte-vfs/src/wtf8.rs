//! The Windows encoding boundary, hoisted (2026-08-10-volumes.md task V4).
//!
//! `norte-vfs-local`'s `native_path` module already trusted one specific
//! fact about `std` on Windows: [`OsStr::as_encoded_bytes`] returns WTF-8
//! (Simon Sapin's UTF-8-plus-lone-surrogates spec) there — never documented
//! as a hard `std` guarantee, but true of every Rust `OsString` backed by a
//! `Vec<u16>` today, and this repository already leans on it
//! (`native_path::os_to_bytes`, since before this module existed) to turn a
//! `Path` component into the bytes a [`norte_proto::VPath`] segment carries.
//! The DECODE direction (WTF-8 bytes → UTF-16, needed to reconstruct an
//! `OsString` from wire bytes without `unsafe`) used to live as a private,
//! hand-rolled codec in that same module — deliberately pure, portable Rust
//! rather than a `cfg(windows)` call into `std`, so it compiles and is
//! TESTED on this repository's Linux gate even though it is only ever USED
//! at the Windows boundary.
//!
//! Both directions now live here, because `norte-core::volumes::windows`
//! (V4) needs BOTH for `Volume::label` (`Volume::label`'s rustdoc has the
//! full per-platform breakdown): `GetVolumeInformationW` hands back UTF-16
//! code units, and encoding them to WTF-8 losslessly — including a lone
//! surrogate, which a FAT/NTFS label field can legally contain — is the
//! ENCODE direction's job. `norte-proto` cannot depend on `norte-vfs-local`
//! (the dependency runs the other way: `norte-vfs-local` implements
//! `norte-vfs`'s `Provider`), so both functions live here, where both
//! `norte-vfs-local` and `norte-core` can reach them.
//!
//! [`encode_from_wide`] follows [`decode_to_wide`]'s own precedent
//! (portable, no `cfg(windows)`, tested on Linux) rather than the simpler
//! `OsStringExt::from_wide` + [`os_to_bytes`] one-liner V4 shipped with
//! first: encoding-auditor review of that first pass found that path had
//! ZERO test coverage on this repository's only gate (Linux; the code only
//! compiled under `cfg(windows)`, and GitHub CI is off), for the exact
//! surrogate-preservation property [`Volume::label`](../../norte_core/volumes/struct.Volume.html)
//! exists to get right — "very likely correct per `std`'s actual internal
//! representation" and "verified by this repository" are different claims,
//! and only the portable form lets this repo make the second one.

use std::ffi::OsStr;

/// The raw bytes backing an `OsStr` — WTF-8 on Windows, the platform's own
/// bytes on Unix (both per [`OsStr::as_encoded_bytes`]'s current, if
/// unspecified-by-contract, behaviour; see the module rustdoc for why this
/// repository already trusts it).
///
/// ```
/// use std::ffi::OsStr;
/// assert_eq!(norte_vfs::wtf8::os_to_bytes(OsStr::new("hola")), b"hola");
/// ```
#[must_use]
pub fn os_to_bytes(os: &OsStr) -> Vec<u8> {
    os.as_encoded_bytes().to_vec()
}

/// WTF-8 (Simon Sapin's spec): UTF-8 plus lone surrogates (`ED A0..BF
/// 80..BF`), forbidding a lead+trail pair encoded that way (canonical WTF-8
/// encodes a real surrogate pair as one 4-byte supplementary-plane
/// sequence, same as UTF-8). Decodes to UTF-16 code units: a lone surrogate
/// survives as its own `D800..DFFF` unit, a supplementary codepoint as a
/// pair. `None` if `b` is not valid WTF-8.
///
/// ```
/// assert_eq!(norte_vfs::wtf8::decode_to_wide(b"ab"), Some(vec![0x61, 0x62]));
/// // A lone lead surrogate, illegal UTF-16 on its own, decodes to its OWN
/// // unit rather than being rejected — that is what WTF-8 is FOR.
/// assert_eq!(
///     norte_vfs::wtf8::decode_to_wide(&[0xED, 0xA0, 0x80]),
///     Some(vec![0xD800])
/// );
/// ```
///
/// # Panics
/// Never: every `u32` this function converts to `u16` is masked or ranged
/// (a WTF-8 sequence's decoded codepoint split into a surrogate pair, or a
/// BMP codepoint under `0x1_0000`) to fit in 16 bits before the conversion
/// is attempted, by construction of the surrounding WTF-8 grammar — the
/// `.expect()`s document that invariant, not an unchecked assumption.
#[must_use]
pub fn decode_to_wide(b: &[u8]) -> Option<Vec<u16>> {
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    // Was the previous sequence a lead surrogate (D800-DBFF)?
    let mut prev_lead = false;
    while i < b.len() {
        let x = b[i];
        let (cp, len) = match x {
            0x00..=0x7F => (u32::from(x), 1),
            0xC2..=0xDF => {
                if !cont(b, i + 1, 1) {
                    return None;
                }
                ((u32::from(x) & 0x1F) << 6 | tail(b[i + 1]), 2)
            }
            0xE0 => {
                if b.len() < i + 3 || !(0xA0..=0xBF).contains(&b[i + 1]) || !is_cont(b[i + 2]) {
                    return None;
                }
                (three(x, b[i + 1], b[i + 2]), 3)
            }
            // 0xED allows a second byte of 80..BF: strict UTF-8 (80..9F) and
            // WTF-8 surrogates (A0..BF) at once.
            0xE1..=0xEF => {
                if !cont(b, i + 1, 2) {
                    return None;
                }
                (three(x, b[i + 1], b[i + 2]), 3)
            }
            0xF0 => {
                if b.len() < i + 4 || !(0x90..=0xBF).contains(&b[i + 1]) || !cont(b, i + 2, 2) {
                    return None;
                }
                (four(x, b[i + 1], b[i + 2], b[i + 3]), 4)
            }
            0xF1..=0xF3 => {
                if !cont(b, i + 1, 3) {
                    return None;
                }
                (four(x, b[i + 1], b[i + 2], b[i + 3]), 4)
            }
            0xF4 => {
                if b.len() < i + 4 || !(0x80..=0x8F).contains(&b[i + 1]) || !cont(b, i + 2, 2) {
                    return None;
                }
                (four(x, b[i + 1], b[i + 2], b[i + 3]), 4)
            }
            _ => return None,
        };
        let lead = (0xD800..=0xDBFF).contains(&cp);
        let trail = (0xDC00..=0xDFFF).contains(&cp);
        if prev_lead && trail {
            // A surrogate pair encoded as CESU-8: WTF-8 forbids this shape.
            return None;
        }
        prev_lead = lead;
        if cp > 0xFFFF {
            let v = cp - 0x1_0000;
            out.push(u16::try_from(0xD800 + (v >> 10)).expect("high surrogate fits in u16"));
            out.push(u16::try_from(0xDC00 + (v & 0x3FF)).expect("low surrogate fits in u16"));
        } else {
            out.push(u16::try_from(cp).expect("BMP codepoint fits in u16"));
        }
        i += len;
    }
    Some(out)
}

/// `true` if `b` is valid WTF-8.
///
/// ```
/// assert!(norte_vfs::wtf8::is_valid("cañón".as_bytes()));
/// assert!(!norte_vfs::wtf8::is_valid(&[0xFF, 0xFE])); // a raw UTF-16 BOM, not WTF-8
/// ```
#[must_use]
pub fn is_valid(b: &[u8]) -> bool {
    decode_to_wide(b).is_some()
}

/// The inverse of [`decode_to_wide`]: UTF-16 code units to WTF-8 bytes. A
/// surrogate pair (lead `D800..DBFF` immediately followed by trail
/// `DC00..DFFF`) becomes ONE 4-byte supplementary-plane UTF-8 sequence; any
/// other unit — including a LONE surrogate a FAT/NTFS volume label or a
/// Windows path segment can legally contain — becomes its own UTF-8-shaped
/// sequence via the same bit-packing UTF-8 uses for any codepoint in its
/// range, which for the surrogate range (`0x800..=0xFFFF`) produces exactly
/// the 3-byte `ED Ax/Bx xx` form [`decode_to_wide`] recognizes as WTF-8's
/// lone-surrogate encoding. This is what lets `norte-core::volumes::windows`
/// preserve an unpaired surrogate from `GetVolumeInformationW` instead of a
/// naive `String::from_utf16_lossy` silently replacing it with `U+FFFD`
/// before rule 1 ever gets a say (`Volume::label`'s rustdoc).
///
/// ```
/// use norte_vfs::wtf8::encode_from_wide;
/// assert_eq!(encode_from_wide(&[0x61, 0x62]), b"ab");
/// // Same lone surrogate as `decode_to_wide`'s doctest, round-tripped.
/// assert_eq!(encode_from_wide(&[0xD800]), vec![0xED, 0xA0, 0x80]);
/// ```
#[must_use]
pub fn encode_from_wide(units: &[u16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(units.len() * 3);
    let mut i = 0;
    while i < units.len() {
        let u = units[i];
        let cp: u32 = if (0xD800..=0xDBFF).contains(&u)
            && i + 1 < units.len()
            && (0xDC00..=0xDFFF).contains(&units[i + 1])
        {
            let lead = u32::from(u);
            let trail = u32::from(units[i + 1]);
            i += 2;
            0x1_0000 + ((lead - 0xD800) << 10) + (trail - 0xDC00)
        } else {
            i += 1;
            u32::from(u)
        };
        push_utf8(&mut out, cp);
    }
    out
}

/// Appends `cp`'s UTF-8 (or, for a lone surrogate, WTF-8) encoding to `out`.
fn push_utf8(out: &mut Vec<u8>, cp: u32) {
    match cp {
        0x00..=0x7F => out.push(u8::try_from(cp).expect("checked <= 0x7F")),
        0x80..=0x7FF => {
            out.push(0xC0 | u8::try_from(cp >> 6).expect("checked <= 0x1F"));
            out.push(0x80 | tail_byte(cp));
        }
        0x800..=0xFFFF => {
            out.push(0xE0 | u8::try_from(cp >> 12).expect("checked <= 0x0F"));
            out.push(0x80 | u8::try_from((cp >> 6) & 0x3F).expect("masked to 6 bits"));
            out.push(0x80 | tail_byte(cp));
        }
        _ => {
            out.push(0xF0 | u8::try_from(cp >> 18).expect("checked <= 0x07"));
            out.push(0x80 | u8::try_from((cp >> 12) & 0x3F).expect("masked to 6 bits"));
            out.push(0x80 | u8::try_from((cp >> 6) & 0x3F).expect("masked to 6 bits"));
            out.push(0x80 | tail_byte(cp));
        }
    }
}

/// The low 6 bits of `cp`, as a UTF-8 continuation byte's payload.
fn tail_byte(cp: u32) -> u8 {
    u8::try_from(cp & 0x3F).expect("masked to 6 bits")
}

fn tail(b: u8) -> u32 {
    u32::from(b) & 0x3F
}

fn three(x: u8, b1: u8, b2: u8) -> u32 {
    (u32::from(x) & 0x0F) << 12 | tail(b1) << 6 | tail(b2)
}

fn four(x: u8, b1: u8, b2: u8, b3: u8) -> u32 {
    (u32::from(x) & 0x07) << 18 | tail(b1) << 12 | tail(b2) << 6 | tail(b3)
}

fn is_cont(b: u8) -> bool {
    (0x80..=0xBF).contains(&b)
}

fn cont(b: &[u8], from: usize, n: usize) -> bool {
    b.len() >= from + n && b[from..from + n].iter().all(|&x| is_cont(x))
}

#[cfg(test)]
mod tests {
    use super::{decode_to_wide, encode_from_wide, is_valid, os_to_bytes};
    use std::ffi::OsStr;

    /// ASCII round-trips identically on every OS — the one `os_to_bytes`
    /// case this test can pin without a Windows target (see the module
    /// rustdoc: the WTF-8 claim itself is only exercised at the real
    /// Windows boundary, which this Linux gate cannot compile — that is
    /// exactly the gap `decode_to_wide`/`encode_from_wide` below do not
    /// have, being portable).
    #[test]
    fn ascii_bytes_pass_through() {
        assert_eq!(os_to_bytes(OsStr::new("USB de Nico")), b"USB de Nico");
    }

    #[test]
    fn utf8_valido_es_wtf8() {
        for s in ["", "abc", "cañón", "テスト", "👨‍👩‍👧‍👦", "\u{10FFFF}"] {
            assert!(is_valid(s.as_bytes()), "{s:?}");
        }
    }

    #[test]
    fn surrogates_sueltos_validos() {
        assert!(is_valid(&[0xED, 0xA0, 0x80])); // lead D800 suelto
        assert!(is_valid(&[0xED, 0xB0, 0x80])); // trail DC00 suelto
        assert!(is_valid(&[0xED, 0xB0, 0x80, 0xED, 0xA0, 0x80])); // trail+lead OK
        assert!(is_valid(&[0xED, 0xA0, 0x80, 0xED, 0xA0, 0x80])); // lead+lead OK
        assert!(is_valid(b"a\xED\xA0\x80b"));
    }

    #[test]
    fn fronteras_ed_y_e0() {
        assert!(is_valid(&[0xED, 0x9F, 0xBF])); // U+D7FF: UTF-8 legal
        assert!(is_valid(&[0xEE, 0x80, 0x80])); // U+E000: tras surrogates
        assert!(!is_valid(&[0xE0, 0x9F, 0x80])); // overlong de 3 bytes
    }

    #[test]
    fn decode_produce_utf16_correcto() {
        assert_eq!(decode_to_wide(b"ab").unwrap(), vec![0x61, 0x62]);
        // é U+00E9
        assert_eq!(decode_to_wide("é".as_bytes()).unwrap(), vec![0x00E9]);
        // 👨 U+1F468 → par de surrogates
        assert_eq!(
            decode_to_wide("👨".as_bytes()).unwrap(),
            vec![0xD83D, 0xDC68]
        );
        // lead surrogate suelto queda como su unidad
        assert_eq!(decode_to_wide(&[0xED, 0xA0, 0x80]).unwrap(), vec![0xD800]);
        // U+10FFFF → último par válido
        assert_eq!(
            decode_to_wide("\u{10FFFF}".as_bytes()).unwrap(),
            vec![0xDBFF, 0xDFFF]
        );
    }

    #[test]
    fn par_cesu8_invalido() {
        // lead + trail consecutivos: en WTF-8 canónico sería 4 bytes.
        assert!(!is_valid(&[0xED, 0xA0, 0x80, 0xED, 0xB0, 0x80]));
    }

    #[test]
    fn basura_invalida() {
        for bad in [
            &[0xC0, 0xAF][..],       // overlong
            &[0xE0, 0x80, 0x80][..], // overlong 3 bytes
            &[0xF5, 0x80, 0x80, 0x80][..],
            &[0x80][..],                   // continuación suelta
            &[0xC2][..],                   // truncado
            &[0xE9][..],                   // latin1 crudo
            &[0xFF, 0xFE][..],             // BOM UTF-16
            &[0xF4, 0x90, 0x80, 0x80][..], // > U+10FFFF
        ] {
            assert!(!is_valid(bad), "{bad:02X?}");
        }
    }

    /// `encode_from_wide` is `decode_to_wide`'s exact inverse — pinned
    /// directly (not just via the corpus round-trip below) because these
    /// are the two textbook shapes a hand-rolled encoder gets wrong: a
    /// BMP codepoint and a real surrogate PAIR (as opposed to two lone
    /// surrogates that happen to sit next to each other, which must NOT
    /// collapse into a pair — `basura_invalida`/`par_cesu8_invalido` above
    /// pin the decode side of that same distinction).
    #[test]
    fn encode_es_el_inverso_exacto_de_decode() {
        assert_eq!(encode_from_wide(&[0x61, 0x62]), b"ab");
        assert_eq!(encode_from_wide(&[0x00E9]), "é".as_bytes());
        assert_eq!(encode_from_wide(&[0xD83D, 0xDC68]), "👨".as_bytes());
        assert_eq!(encode_from_wide(&[0xDBFF, 0xDFFF]), "\u{10FFFF}".as_bytes());
    }

    /// The finding this test exists to close: encoding-auditor review of
    /// V4's first pass found the encode direction (needed for
    /// `Volume::label` on Windows) had ZERO test coverage anywhere in this
    /// repository — it only compiled under `cfg(windows)`, and this repo's
    /// gate is Linux with GitHub CI off, so nothing would ever have caught
    /// a regression. `encode_from_wide` is portable (no `cfg`), so THIS
    /// runs on the Linux gate, round-tripping every hostile-corpus fixture
    /// through decode→encode — including `lone_surrogate`
    /// (`crates/norte-testkit/src/corpus/names.json`, bytes `ED A0 80`),
    /// the exact byte sequence a real FAT/NTFS volume label could contain.
    /// Fixtures that are not valid WTF-8 in the first place (e.g. a raw
    /// Latin-1 byte, or an overlong encoding) are skipped: there is nothing
    /// for `decode_to_wide` to hand `encode_from_wide` to round-trip.
    #[test]
    fn encode_redondea_el_corpus_hostil_completo() {
        for fixture in norte_testkit::corpus::hostile_names() {
            let Some(wide) = decode_to_wide(&fixture.bytes) else {
                continue;
            };
            assert_eq!(
                encode_from_wide(&wide),
                fixture.bytes,
                "{}: encode(decode(bytes)) != bytes",
                fixture.id,
            );
        }
    }
}
