//! Conversión `VPath` ↔ paths nativos y prefijo `\\?\` (paths >260, nombres
//! reservados, trailing dots/spaces).
//!
//! Frontera de seguridad (ADR 0001): en Windows los bytes de un segmento se
//! validan como WTF-8 y se DECODIFICAN a UTF-16 (`OsStringExt::from_wide`) —
//! cero `unsafe`: la reconstrucción unchecked de `OsStr` queda prohibida
//! porque su contrato ("bytes de `as_encoded_bytes` de la misma versión de
//! Rust") no cubre bytes llegados del wire.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use norte_proto::{Error, VPath};

/// Bytes crudos de un `OsStr` (la forma que guarda `Segment`).
///
/// Unix: los bytes del OS tal cual. Windows: WTF-8 (`as_encoded_bytes`).
pub(crate) fn os_to_bytes(os: &OsStr) -> Vec<u8> {
    os.as_encoded_bytes().to_vec()
}

/// Reconstruye un `OsString` desde los bytes de un segmento.
///
/// # Errors
/// [`Error::InvalidPath`] en Windows si los bytes no son WTF-8 válido
/// (imposible como nombre de archivo Windows; además la reconstrucción
/// unchecked sería unsound).
#[cfg(unix)]
#[allow(clippy::unnecessary_wraps)] // firma común con la variante Windows, que sí falla
pub(crate) fn bytes_to_os(bytes: &[u8]) -> Result<OsString, Error> {
    use std::os::unix::ffi::OsStrExt;
    // Unix: cualquier byte es válido en un nombre; conversión segura 1:1.
    Ok(OsStr::from_bytes(bytes).to_os_string())
}

/// Reconstruye un `OsString` desde los bytes de un segmento (Windows: WTF-8
/// validado → UTF-16 → `from_wide`, sin `unsafe`).
///
/// # Errors
/// [`Error::InvalidPath`] si los bytes no son WTF-8 válido, o si contienen
/// `\` (separador también bajo `\\?\`: un segmento produciría DOS
/// componentes) o `:` (Alternate Data Stream de NTFS: los datos acabarían
/// escondidos en un stream que `list` jamás devuelve).
#[cfg(windows)]
pub(crate) fn bytes_to_os(bytes: &[u8]) -> Result<OsString, Error> {
    use std::os::windows::ffi::OsStringExt;
    if bytes.contains(&b'\\') || bytes.contains(&b':') {
        return Err(Error::InvalidPath);
    }
    let wide = wtf8::decode_to_wide(bytes).ok_or(Error::InvalidPath)?;
    Ok(OsString::from_wide(&wide))
}

/// Path nativo de `p` bajo `base`: `base/<seg1>/<seg2>/…`.
///
/// En Windows el resultado va SIEMPRE con prefijo verbatim `\\?\` (paths
/// >260, `CON`/`NUL`, trailing dots/spaces intactos).
pub(crate) fn to_native(base: &Path, p: &VPath) -> Result<PathBuf, Error> {
    let mut out = base.to_path_buf();
    for seg in p.segments() {
        out.push(bytes_to_os(seg)?);
    }
    Ok(verbatim(out))
}

/// Aplica el prefijo verbatim en Windows; identidad en el resto.
#[cfg(not(windows))]
pub(crate) fn verbatim(p: PathBuf) -> PathBuf {
    p
}

/// Aplica el prefijo verbatim en Windows; identidad en el resto.
#[cfg(windows)]
pub(crate) fn verbatim(p: PathBuf) -> PathBuf {
    use std::path::{Component, Prefix};
    // Ya verbatim: no tocar.
    if let Some(Component::Prefix(pr)) = p.components().next() {
        match pr.kind() {
            Prefix::Verbatim(_) | Prefix::VerbatimUNC(..) | Prefix::VerbatimDisk(_) => return p,
            Prefix::UNC(server, share) => {
                // \\server\share\… → \\?\UNC\server\share\…
                let mut out = PathBuf::from(r"\\?\UNC");
                out.push(server);
                out.push(share);
                for c in p.components() {
                    match c {
                        Component::Prefix(_) | Component::RootDir => {}
                        other => out.push(other.as_os_str()),
                    }
                }
                return out;
            }
            _ => {}
        }
    }
    let mut s = OsString::from(r"\\?\");
    s.push(p.as_os_str());
    PathBuf::from(s)
}

/// WTF-8 (spec de Simon Sapin): UTF-8 más surrogates sueltos
/// (`ED A0..BF 80..BF`), prohibiendo pares lead+trail consecutivos (en WTF-8
/// canónico serían una secuencia de 4 bytes). Compilado en todos los OS para
/// poder testearlo en CI de Linux; usado en la frontera de Windows.
pub(crate) mod wtf8 {
    /// Decodifica WTF-8 a unidades UTF-16: surrogates sueltos quedan como su
    /// unidad `D800..DFFF`; codepoints suplementarios, como par. `None` si
    /// los bytes no son WTF-8 válido.
    #[allow(dead_code)] // usado solo bajo cfg(windows); testeado en todos.
    pub(crate) fn decode_to_wide(b: &[u8]) -> Option<Vec<u16>> {
        let mut out = Vec::with_capacity(b.len());
        let mut i = 0;
        // ¿La secuencia anterior fue un lead surrogate (D800–DBFF)?
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
                // 0xED admite segundo byte 80..BF: UTF-8 estricto (80..9F) y
                // surrogates WTF-8 (A0..BF) a la vez.
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
                // Par de surrogates codificado como CESU-8: WTF-8 lo prohíbe.
                return None;
            }
            prev_lead = lead;
            if cp > 0xFFFF {
                let v = cp - 0x1_0000;
                out.push(u16::try_from(0xD800 + (v >> 10)).expect("high surrogate cabe en u16"));
                out.push(u16::try_from(0xDC00 + (v & 0x3FF)).expect("low surrogate cabe en u16"));
            } else {
                out.push(u16::try_from(cp).expect("BMP cabe en u16"));
            }
            i += len;
        }
        Some(out)
    }

    /// `true` si `b` es WTF-8 válido.
    #[allow(dead_code)] // usado solo bajo cfg(windows); testeado en todos.
    pub(crate) fn is_valid(b: &[u8]) -> bool {
        decode_to_wide(b).is_some()
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
        use super::is_valid;

        #[test]
        fn utf8_valido_es_wtf8() {
            for s in ["", "abc", "cañón", "テスト", "👨‍👩‍👧‍👦", "\u{10FFFF}"]
            {
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
            use super::decode_to_wide;
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
    }
}
