//! What the host does with a thumbnail BEFORE it crosses (ADR 0107,
//! decision 3): a guest returns image bytes, and those bytes would end up
//! in a webview `blob:`, where native libpng/libjpeg/libwebp decode
//! them — outside any norte sandbox. Two gates:
//!
//! 1. [`sniff`]: only the header — magic bytes and dimensions — of the
//!    three encodings the window paints. Cheap, and enough to reject
//!    something that doesn't even claim to be an image or lies about what
//!    it is.
//! 2. [`reencode`]: it is DECODED on the host with the `image` crate (safe
//!    Rust, with size limits) and re-encoded. What reaches the `blob:` is
//!    a raster made here; the guest's bytes never leave the process. A
//!    truthful header over a malformed compressed stream — the polyglot
//!    that passes gate 1 — dies in a Rust decoder, not a C one with the
//!    desktop behind it.

use std::io::Cursor;

/// The largest edge asked of a guest, at most (mirrors
/// `runtime::THUMB_MAX_EDGE`, here for the decoder's limits).
const MAX_EDGE: u32 = 2048;

/// Memory the host's decoder can ask for per thumbnail: a 2048×2048 RGBA
/// is 16 MiB; double that leaves room for the codec's tables.
const MAX_DECODE_ALLOC: u64 = 32 * 1024 * 1024;

/// Re-encodes `bytes` (already passed through [`sniff`]) on the host:
/// decodes with limits, checks the decoded dimensions are `w`×`h`, and
/// writes PNG — or quality-85 JPEG if the PNG does not fit in `max_bytes`
/// (a 2048 px photo in PNG is ten megabytes; the same raster in JPEG,
/// one). Returns the resulting raster's mimetype and its bytes.
///
/// # Errors
/// The decoder's or encoder's message, for the host's log; the thumbnail
/// then does not cross.
pub fn reencode(
    bytes: &[u8],
    w: u32,
    h: u32,
    max_bytes: usize,
) -> Result<(&'static str, Vec<u8>), String> {
    let mut reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| format!("header: {e}"))?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_EDGE);
    limits.max_image_height = Some(MAX_EDGE);
    limits.max_alloc = Some(MAX_DECODE_ALLOC);
    reader.limits(limits);
    let img = reader.decode().map_err(|e| format!("decode: {e}"))?;
    if (img.width(), img.height()) != (w, h) {
        return Err(format!(
            "the header said {w}x{h} and the raster is {}x{}",
            img.width(),
            img.height()
        ));
    }
    let mut png = Cursor::new(Vec::new());
    img.write_to(&mut png, image::ImageFormat::Png)
        .map_err(|e| format!("encode png: {e}"))?;
    let png = png.into_inner();
    if png.len() <= max_bytes {
        return Ok(("image/png", png));
    }
    let rgb = img.to_rgb8();
    let mut jpeg = Cursor::new(Vec::new());
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 85)
        .encode_image(&rgb)
        .map_err(|e| format!("encode jpeg: {e}"))?;
    let jpeg = jpeg.into_inner();
    if jpeg.len() > max_bytes {
        return Err(format!(
            "does not fit even in JPEG: {} bytes with a ceiling of {max_bytes}",
            jpeg.len()
        ));
    }
    Ok(("image/jpeg", jpeg))
}

/// The encodings the window paints, with their mimetype.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThumbKind {
    Png,
    Jpeg,
    Webp,
}

impl ThumbKind {
    /// The mimetype the guest must have declared for this raster.
    #[must_use]
    pub const fn mimetype(self) -> &'static str {
        match self {
            ThumbKind::Png => "image/png",
            ThumbKind::Jpeg => "image/jpeg",
            ThumbKind::Webp => "image/webp",
        }
    }
}

/// What the header says: the encoding by its magic bytes and the
/// dimensions. `None` if it is none of the three or the header does not
/// arrive whole.
#[must_use]
pub fn sniff(bytes: &[u8]) -> Option<(ThumbKind, u32, u32)> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        // IHDR is ALWAYS the first chunk: signature (8) + length (4) +
        // type (4) + width (4) + height (4).
        if bytes.len() < 24 || &bytes[12..16] != b"IHDR" {
            return None;
        }
        let w = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
        let h = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
        return Some((ThumbKind::Png, w, h));
    }
    if bytes.starts_with(&[0xFF, 0xD8]) {
        return jpeg_dims(bytes).map(|(w, h)| (ThumbKind::Jpeg, w, h));
    }
    if bytes.len() >= 16 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return webp_dims(bytes).map(|(w, h)| (ThumbKind::Webp, w, h));
    }
    None
}

/// Walks the markers up to the first SOF (`FFC0`–`FFCF`, except the ones
/// that are not frames: `C4` DHT, `C8` JPG, `CC` DAC) and reads height and
/// width.
fn jpeg_dims(bytes: &[u8]) -> Option<(u32, u32)> {
    let mut i = 2usize;
    while i + 4 <= bytes.len() {
        if bytes[i] != 0xFF {
            return None;
        }
        let marker = bytes[i + 1];
        // `FF FF …` padding between markers.
        if marker == 0xFF {
            i += 1;
            continue;
        }
        // No payload: SOI, EOI, RSTn, TEM.
        if marker == 0xD8 || marker == 0xD9 || (0xD0..=0xD7).contains(&marker) || marker == 0x01 {
            i += 2;
            continue;
        }
        let len = usize::from(u16::from_be_bytes([bytes[i + 2], bytes[i + 3]]));
        if len < 2 {
            return None;
        }
        let is_sof = (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC);
        if is_sof {
            // Length (2) + precision (1) + height (2) + width (2).
            if i + 9 > bytes.len() {
                return None;
            }
            let h = u32::from(u16::from_be_bytes([bytes[i + 5], bytes[i + 6]]));
            let w = u32::from(u16::from_be_bytes([bytes[i + 7], bytes[i + 8]]));
            return Some((w, h));
        }
        i += 2 + len;
    }
    None
}

/// The three WebP flavors: `VP8 ` (lossy), `VP8L` (lossless) and `VP8X`
/// (extended, with the canvas in the header itself).
fn webp_dims(bytes: &[u8]) -> Option<(u32, u32)> {
    let chunk = &bytes[12..16];
    match chunk {
        b"VP8X" => {
            // Canvas: 24 bits width-1 and 24 bits height-1, after 4 bytes
            // of flags.
            if bytes.len() < 30 {
                return None;
            }
            let w = 1 + u32::from_le_bytes([bytes[24], bytes[25], bytes[26], 0]);
            let h = 1 + u32::from_le_bytes([bytes[27], bytes[28], bytes[29], 0]);
            Some((w, h))
        }
        b"VP8L" => {
            // Signature 0x2F and 14 bits width-1, 14 height-1.
            if bytes.len() < 25 || bytes[20] != 0x2F {
                return None;
            }
            let b = u32::from_le_bytes([bytes[21], bytes[22], bytes[23], bytes[24]]);
            Some((1 + (b & 0x3FFF), 1 + ((b >> 14) & 0x3FFF)))
        }
        b"VP8 " => {
            // Frame tag (3) + start code 9d 01 2a (3) + width (2) + height
            // (2), the low 14 bits of each.
            if bytes.len() < 30 || &bytes[23..26] != b"\x9d\x01\x2a" {
                return None;
            }
            let w = u32::from(u16::from_le_bytes([bytes[26], bytes[27]]) & 0x3FFF);
            let h = u32::from(u16::from_le_bytes([bytes[28], bytes[29]]) & 0x3FFF);
            Some((w, h))
        }
        _ => None,
    }
}

#[cfg(test)]
#[expect(
    clippy::cast_possible_truncation,
    reason = "toy rasters: pixels are generated with modular arithmetic"
)]
mod tests {
    use super::*;

    fn png(w: u32, h: u32) -> Vec<u8> {
        let mut v = b"\x89PNG\r\n\x1a\n".to_vec();
        v.extend_from_slice(&13u32.to_be_bytes());
        v.extend_from_slice(b"IHDR");
        v.extend_from_slice(&w.to_be_bytes());
        v.extend_from_slice(&h.to_be_bytes());
        v.extend_from_slice(&[8, 2, 0, 0, 0]);
        v
    }

    #[test]
    fn png_dims_come_from_ihdr_and_a_cut_header_is_nothing() {
        assert_eq!(sniff(&png(640, 480)), Some((ThumbKind::Png, 640, 480)));
        assert_eq!(sniff(&png(640, 480)[..20]), None);
        assert_eq!(sniff(b"\x89PNG\r\n\x1a\nxxxxIDAT\0\0\0\0\0\0\0\0"), None);
    }

    #[test]
    fn jpeg_dims_come_from_the_first_sof_past_app_segments() {
        // SOI, 16-byte APP0, DHT (not a frame), SOF0 320x200.
        let mut v = vec![0xFF, 0xD8];
        v.extend_from_slice(&[0xFF, 0xE0, 0x00, 0x10]);
        v.extend_from_slice(&[0u8; 14]);
        v.extend_from_slice(&[0xFF, 0xC4, 0x00, 0x04, 0, 0]);
        v.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x11, 8]);
        v.extend_from_slice(&200u16.to_be_bytes());
        v.extend_from_slice(&320u16.to_be_bytes());
        v.extend_from_slice(&[3, 1, 0x22, 0, 2, 0x11, 1, 3, 0x11, 1]);
        assert_eq!(sniff(&v), Some((ThumbKind::Jpeg, 320, 200)));
        assert_eq!(sniff(&[0xFF, 0xD8, 0xFF, 0xE0]), None, "no SOF");
    }

    #[test]
    fn webp_dims_for_the_three_flavours() {
        let mut lossy = b"RIFF\0\0\0\0WEBPVP8 \0\0\0\0".to_vec();
        lossy.extend_from_slice(&[0, 0, 0, 0x9d, 0x01, 0x2a]);
        lossy.extend_from_slice(&100u16.to_le_bytes());
        lossy.extend_from_slice(&50u16.to_le_bytes());
        assert_eq!(sniff(&lossy), Some((ThumbKind::Webp, 100, 50)));

        let mut lossless = b"RIFF\0\0\0\0WEBPVP8L\0\0\0\0".to_vec();
        lossless.push(0x2F);
        let bits: u32 = (100 - 1) | ((50 - 1) << 14);
        lossless.extend_from_slice(&bits.to_le_bytes());
        assert_eq!(sniff(&lossless), Some((ThumbKind::Webp, 100, 50)));

        let mut ext = b"RIFF\0\0\0\0WEBPVP8X\0\0\0\0".to_vec();
        ext.extend_from_slice(&[0, 0, 0, 0]);
        ext.extend_from_slice(&[99, 0, 0, 49, 0, 0]);
        assert_eq!(sniff(&ext), Some((ThumbKind::Webp, 100, 50)));
    }

    /// A real PNG, made with the same crate: gate 2 has to DECODE, and a
    /// header alone is not enough.
    fn png_real(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbaImage::from_fn(w, h, |x, y| {
            image::Rgba([(x % 256) as u8, (y % 256) as u8, 7, 255])
        });
        let mut out = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut out, image::ImageFormat::Png)
            .expect("png");
        out.into_inner()
    }

    #[test]
    fn reencode_makes_a_host_png_and_refuses_a_lying_header_or_a_broken_stream() {
        let bytes = png_real(64, 32);
        let (mime, out) = reencode(&bytes, 64, 32, 4 * 1024 * 1024).expect("re-encode");
        assert_eq!(mime, "image/png");
        assert!(out.starts_with(b"\x89PNG"));
        assert_eq!(sniff(&out), Some((ThumbKind::Png, 64, 32)));
        // Truthful header over a broken stream: passes `sniff`, dies here.
        let mut broken = bytes.clone();
        for b in broken.iter_mut().skip(40) {
            *b = 0xAA;
        }
        assert_eq!(sniff(&broken).map(|(k, _, _)| k), Some(ThumbKind::Png));
        assert!(reencode(&broken, 64, 32, 4 * 1024 * 1024).is_err());
        // Dimensions that are not the decoded ones.
        assert!(reencode(&bytes, 32, 64, 4 * 1024 * 1024).is_err());
    }

    #[test]
    fn reencode_falls_back_to_jpeg_when_png_does_not_fit() {
        // Noise: PNG doesn't compress, and a small ceiling pushes it to
        // JPEG.
        let img = image::RgbaImage::from_fn(256, 256, |x, y| {
            let v = (x.wrapping_mul(2_654_435_761) ^ y.wrapping_mul(40_503)) as u8;
            image::Rgba([v, v.wrapping_mul(3), v.wrapping_mul(7), 255])
        });
        let mut out = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut out, image::ImageFormat::Png)
            .expect("png");
        let (mime, bytes) =
            reencode(&out.into_inner(), 256, 256, 120 * 1024).expect("fits in jpeg");
        assert_eq!(mime, "image/jpeg");
        assert_eq!(sniff(&bytes), Some((ThumbKind::Jpeg, 256, 256)));
    }

    #[test]
    fn anything_else_is_not_a_thumbnail() {
        assert_eq!(sniff(b"GIF89a"), None);
        assert_eq!(sniff(b""), None);
        assert_eq!(sniff(b"BM"), None);
    }
}
