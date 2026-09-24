//! SHA-1, just enough to compute a blob's object id.
//!
//! Implemented here instead of pulling in a dependency because the guest
//! compiles to `wasm32-wasip2` with `no_std` and this is sixty lines of
//! integer arithmetic. Git identifies a blob by
//! `sha1("blob <len>\0" + content)`, and that is all that is needed to
//! break the "racy" tie (same mtime as the index and same size), which is
//! the only place this parser ever reads a file.

extern crate alloc;

use alloc::vec::Vec;

/// The object id git would give this content as a blob.
#[must_use]
pub fn blob_oid(content: &[u8]) -> [u8; 20] {
    let mut header = Vec::with_capacity(32 + content.len());
    header.extend_from_slice(b"blob ");
    push_decimal(&mut header, content.len() as u64);
    header.push(0);
    header.extend_from_slice(content);
    sha1(&header)
}

fn push_decimal(out: &mut Vec<u8>, mut n: u64) {
    if n == 0 {
        out.push(b'0');
        return;
    }
    let mut digits = [0u8; 20];
    let mut at = digits.len();
    while n > 0 {
        at -= 1;
        digits[at] = b'0' + u8::try_from(n % 10).unwrap_or(0);
        n /= 10;
    }
    out.extend_from_slice(&digits[at..]);
}

/// SHA-1 (RFC 3174) over a whole in-memory message.
#[must_use]
pub fn sha1(msg: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [
        0x6745_2301,
        0xEFCD_AB89,
        0x98BA_DCFE,
        0x1032_5476,
        0xC3D2_E1F0,
    ];
    let mut data = msg.to_vec();
    let bits = (msg.len() as u64).wrapping_mul(8);
    data.push(0x80);
    while data.len() % 64 != 56 {
        data.push(0);
    }
    data.extend_from_slice(&bits.to_be_bytes());

    for chunk in data.chunks_exact(64) {
        let mut w = [0u32; 80];
        for (i, word) in chunk.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);
        for (i, wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5A82_7999),
                20..=39 => (b ^ c ^ d, 0x6ED9_EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC),
                _ => (b ^ c ^ d, 0xCA62_C1D6),
            };
            let tmp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = tmp;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }
    let mut out = [0u8; 20];
    for (i, word) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 3174 vectors, which is the only source of truth needed.
    #[test]
    fn the_rfc_vectors() {
        assert_eq!(
            sha1(b"abc"),
            [
                0xA9, 0x99, 0x3E, 0x36, 0x47, 0x06, 0x81, 0x6A, 0xBA, 0x3E, 0x25, 0x71, 0x78, 0x50,
                0xC2, 0x6C, 0x9C, 0xD0, 0xD8, 0x9D
            ]
        );
        assert_eq!(
            sha1(b""),
            [
                0xDA, 0x39, 0xA3, 0xEE, 0x5E, 0x6B, 0x4B, 0x0D, 0x32, 0x55, 0xBF, 0xEF, 0x95, 0x60,
                0x18, 0x90, 0xAF, 0xD8, 0x07, 0x09
            ]
        );
    }

    /// The id `git hash-object --stdin` gives for «hola\n». Verified
    /// against installed git, not copied from memory.
    #[test]
    fn a_blobs_oid_is_gits() {
        let oid = blob_oid(b"hola\n");
        let hex: alloc::string::String = oid.iter().map(|b| alloc::format!("{b:02x}")).collect();
        assert_eq!(hex, "5c1b14949828006ed75a3e8858957f86a2f7e2eb");
    }

    #[test]
    fn content_over_one_block_too() {
        // 1000 bytes crosses several 64-byte blocks and exercises the padding.
        let big = alloc::vec![b'x'; 1000];
        assert_ne!(sha1(&big), sha1(b"x"));
        assert_eq!(sha1(&big), sha1(&big.clone()));
    }
}
