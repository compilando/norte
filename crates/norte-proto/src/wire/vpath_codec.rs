//! Percent-encoding of `VPath` segments (ADR 0001).
//!
//! Encode rules (bytes → wire):
//! - Valid UTF-8 sequences go literal.
//! - Every byte outside a valid UTF-8 sequence → `%XX` (uppercase hex).
//! - A literal `%` → `%25` (anti-ambiguity).
//! - C0 controls (0x00–0x1F) and DEL (0x7F) → `%XX` even if valid UTF-8: a
//!   wire never carries raw control bytes (terminal injection in logs).
//!
//! Decode rules (wire → bytes):
//! - `%XX` (hex in either case) → raw byte.
//! - A malformed escape (`%G1`, `%4`, a trailing `%`) → [`VPathError::BadEscape`];
//!   never a silent loss.
//! - Lenient with non-canonical forms (`%41` ≡ `A`): the guaranteed roundtrip
//!   is bytes → wire → bytes.

use crate::vpath::VPathError;

const HEX_UPPER: &[u8; 16] = b"0123456789ABCDEF";

/// Encodes a segment's raw bytes into `out`.
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
                // Invariant: `valid_up_to` delimits valid UTF-8 by `Utf8Error`'s contract.
                push_utf8(
                    std::str::from_utf8(valid).expect("valid_up_to guarantees UTF-8"),
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

/// Decodes a wire segment into its raw bytes.
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
            // Invariant: `%` and ASCII controls (C0 + DEL) fit in a u8.
            push_escape(u8::try_from(c).expect("ASCII char"), out);
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
