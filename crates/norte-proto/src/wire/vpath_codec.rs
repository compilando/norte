//! Percent-encoding de segmentos de `VPath` (ADR 0001).
//!
//! Reglas de encode (bytes → wire):
//! - Secuencias UTF-8 válidas van literales.
//! - Todo byte fuera de una secuencia UTF-8 válida → `%XX` (hex mayúscula).
//! - `%` literal → `%25` (anti-ambigüedad).
//! - Controles C0 (0x00–0x1F) y DEL (0x7F) → `%XX` aunque sean UTF-8 válido:
//!   un wire jamás lleva bytes de control crudos (terminal injection en logs).
//!
//! Reglas de decode (wire → bytes):
//! - `%XX` (hex en cualquier caja) → byte crudo.
//! - Escape malformado (`%G1`, `%4`, `%` final) → [`VPathError::BadEscape`];
//!   jamás pérdida silenciosa.
//! - Leniente con formas no canónicas (`%41` ≡ `A`): el roundtrip garantizado
//!   es bytes → wire → bytes.

use crate::vpath::VPathError;

const HEX_UPPER: &[u8; 16] = b"0123456789ABCDEF";

/// Codifica los bytes crudos de un segmento sobre `out`.
pub(crate) fn encode_segment(bytes: &[u8], out: &mut String) {
    let mut rest = bytes;
    while !rest.is_empty() {
        match std::str::from_utf8(rest) {
            Ok(valid) => {
                push_utf8(valid, out);
                break;
            }
            Err(e) => {
                let (valid, invalid) = rest.split_at(e.valid_up_to());
                // Invariante: `valid_up_to` delimita UTF-8 válido por contrato de Utf8Error.
                push_utf8(
                    std::str::from_utf8(valid).expect("valid_up_to garantiza UTF-8"),
                    out,
                );
                let bad_len = e.error_len().unwrap_or(invalid.len());
                for b in &invalid[..bad_len] {
                    push_escape(*b, out);
                }
                rest = &invalid[bad_len..];
            }
        }
    }
}

/// Decodifica un segmento del wire a sus bytes crudos.
pub(crate) fn decode_segment(raw: &str) -> Result<Vec<u8>, VPathError> {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hi = hex_val(*bytes.get(i + 1).ok_or(VPathError::BadEscape)?)?;
            let lo = hex_val(*bytes.get(i + 2).ok_or(VPathError::BadEscape)?)?;
            out.push((hi << 4) | lo);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    Ok(out)
}

fn push_utf8(s: &str, out: &mut String) {
    for c in s.chars() {
        if c == '%' || c.is_ascii_control() {
            // Invariante: `%` y los controles ASCII (C0 + DEL) caben en u8.
            push_escape(u8::try_from(c).expect("char ASCII"), out);
        } else {
            out.push(c);
        }
    }
}

fn push_escape(b: u8, out: &mut String) {
    out.push('%');
    out.push(char::from(HEX_UPPER[usize::from(b >> 4)]));
    out.push(char::from(HEX_UPPER[usize::from(b & 0x0F)]));
}

fn hex_val(b: u8) -> Result<u8, VPathError> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(b - b'a' + 10),
        b'A'..=b'F' => Ok(b - b'A' + 10),
        _ => Err(VPathError::BadEscape),
    }
}
