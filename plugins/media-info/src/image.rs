//! Image dimensions from a header prefix. PNG, JPEG, GIF, WebP.
//!
//! Every function takes whatever bytes the host handed over — a prefix, not
//! the file — and answers `None` for anything it cannot prove: a truncated
//! header, a format it does not know, a JPEG whose frame marker sits past
//! the prefix. An empty cell is "cannot tell", never a guess.

fn be16(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from(*b.get(at)?) << 8 | u32::from(*b.get(at + 1)?))
}

fn be32(b: &[u8], at: usize) -> Option<u32> {
    Some((be16(b, at)? << 16) | be16(b, at + 2)?)
}

fn le16(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from(*b.get(at)?) | u32::from(*b.get(at + 1)?) << 8)
}

fn le24(b: &[u8], at: usize) -> Option<u32> {
    Some(le16(b, at)? | u32::from(*b.get(at + 2)?) << 16)
}

/// `(width, height)` of an image whose header starts `bytes`, or `None`.
pub fn dims(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return png(bytes);
    }
    if bytes.starts_with(&[0xff, 0xd8]) {
        return jpeg(bytes);
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return gif(bytes);
    }
    if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        return webp(bytes);
    }
    None
}

/// IHDR is the first chunk by specification: length, `IHDR`, then width and
/// height, big-endian.
fn png(b: &[u8]) -> Option<(u32, u32)> {
    if b.get(12..16) != Some(b"IHDR") {
        return None;
    }
    let (w, h) = (be32(b, 16)?, be32(b, 20)?);
    (w > 0 && h > 0).then_some((w, h))
}

/// Walk the marker segments until a start-of-frame: height, then width.
fn jpeg(b: &[u8]) -> Option<(u32, u32)> {
    let mut pos = 2;
    loop {
        if *b.get(pos)? != 0xff {
            return None;
        }
        let mut marker = *b.get(pos + 1)?;
        // Fill bytes.
        while marker == 0xff {
            pos += 1;
            marker = *b.get(pos + 1)?;
        }
        match marker {
            // SOF0..SOF15, minus the ones that are not frames (DHT, JPG, DAC).
            0xc0..=0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf => {
                let h = be16(b, pos + 5)?;
                let w = be16(b, pos + 7)?;
                return (w > 0 && h > 0).then_some((w, h));
            }
            // Standalone markers without a length.
            0xd0..=0xd9 | 0x01 => pos += 2,
            _ => {
                let len = be16(b, pos + 2)? as usize;
                if len < 2 {
                    return None;
                }
                pos += 2 + len;
            }
        }
    }
}

fn gif(b: &[u8]) -> Option<(u32, u32)> {
    let (w, h) = (le16(b, 6)?, le16(b, 8)?);
    (w > 0 && h > 0).then_some((w, h))
}

/// Three flavours share the container: lossy `VP8 `, lossless `VP8L`, and
/// the extended `VP8X` whose canvas size is what a viewer shows.
fn webp(b: &[u8]) -> Option<(u32, u32)> {
    match b.get(12..16)? {
        b"VP8 " => {
            if b.get(23..26) != Some(&[0x9d, 0x01, 0x2a]) {
                return None;
            }
            let w = le16(b, 26)? & 0x3fff;
            let h = le16(b, 28)? & 0x3fff;
            (w > 0 && h > 0).then_some((w, h))
        }
        b"VP8L" => {
            if *b.get(20)? != 0x2f {
                return None;
            }
            let bits = u32::from(*b.get(21)?)
                | u32::from(*b.get(22)?) << 8
                | u32::from(*b.get(23)?) << 16
                | u32::from(*b.get(24)?) << 24;
            let w = (bits & 0x3fff) + 1;
            let h = ((bits >> 14) & 0x3fff) + 1;
            Some((w, h))
        }
        b"VP8X" => {
            let w = le24(b, 24)? + 1;
            let h = le24(b, 27)? + 1;
            Some((w, h))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A PNG header claiming `w`×`h`: signature, IHDR length, type, fields.
    pub fn png_header(w: u32, h: u32) -> Vec<u8> {
        let mut v = b"\x89PNG\r\n\x1a\n".to_vec();
        v.extend_from_slice(&13u32.to_be_bytes());
        v.extend_from_slice(b"IHDR");
        v.extend_from_slice(&w.to_be_bytes());
        v.extend_from_slice(&h.to_be_bytes());
        v.extend_from_slice(&[8, 2, 0, 0, 0]);
        v
    }

    #[test]
    fn png_reads_ihdr_and_refuses_a_cut_header() {
        assert_eq!(dims(&png_header(1, 1)), Some((1, 1)));
        assert_eq!(dims(&png_header(1920, 1080)), Some((1920, 1080)));
        assert_eq!(dims(&png_header(8, 4)[..20]), None, "cut before height");
        assert_eq!(dims(b"\x89PNG"), None);
    }

    #[test]
    fn jpeg_walks_segments_to_the_frame() {
        let mut v = vec![0xff, 0xd8];
        // APP0, 16 bytes of payload.
        v.extend_from_slice(&[0xff, 0xe0, 0x00, 0x10]);
        v.extend_from_slice(&[0u8; 14]);
        // SOF0: length 17, precision 8, height 480, width 640.
        v.extend_from_slice(&[0xff, 0xc0, 0x00, 0x11, 0x08]);
        v.extend_from_slice(&480u16.to_be_bytes());
        v.extend_from_slice(&640u16.to_be_bytes());
        assert_eq!(dims(&v), Some((640, 480)));
        assert_eq!(dims(&v[..v.len() - 2]), None, "frame cut short");
        assert_eq!(dims(&[0xff, 0xd8, 0x00]), None, "not a marker");
    }

    #[test]
    fn gif_reads_the_logical_screen() {
        let mut v = b"GIF89a".to_vec();
        v.extend_from_slice(&16u16.to_le_bytes());
        v.extend_from_slice(&8u16.to_le_bytes());
        assert_eq!(dims(&v), Some((16, 8)));
        assert_eq!(dims(b"GIF89a\x10"), None);
    }

    #[test]
    fn webp_reads_all_three_flavours() {
        let riff = |fourcc: &[u8], body: &[u8]| {
            let mut v = b"RIFF\0\0\0\0WEBP".to_vec();
            v.extend_from_slice(fourcc);
            v.extend_from_slice(&[0, 0, 0, 0]);
            v.extend_from_slice(body);
            v
        };
        // VP8X: 24-bit width-1 and height-1 after 4 flag/reserved bytes.
        let mut body = vec![0u8; 4];
        body.extend_from_slice(&99u32.to_le_bytes()[..3]);
        body.extend_from_slice(&49u32.to_le_bytes()[..3]);
        assert_eq!(dims(&riff(b"VP8X", &body)), Some((100, 50)));
        // VP8L: 0x2f then 14-bit fields, minus one.
        let bits: u32 = (300 - 1) | ((200 - 1) << 14);
        let mut body = vec![0x2f];
        body.extend_from_slice(&bits.to_le_bytes());
        assert_eq!(dims(&riff(b"VP8L", &body)), Some((300, 200)));
        // VP8: 3 frame-tag bytes, start code, then 14-bit width and height.
        let mut body = vec![0, 0, 0, 0x9d, 0x01, 0x2a];
        body.extend_from_slice(&640u16.to_le_bytes());
        body.extend_from_slice(&360u16.to_le_bytes());
        assert_eq!(dims(&riff(b"VP8 ", &body)), Some((640, 360)));
        assert_eq!(dims(&riff(b"VP8 ", &[0, 0, 0])), None);
    }
}
