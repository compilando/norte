//! Lo que el host hace con una miniatura ANTES de que cruce (ADR 0107,
//! decisión 3): un guest devuelve bytes de imagen, y esos bytes acabarían
//! en un `blob:` de la webview, donde los decodifica libpng/libjpeg/libwebp
//! nativos —fuera de cualquier sandbox de norte—. Dos puertas:
//!
//! 1. [`sniff`]: solo la cabecera —magia y dimensiones— de los tres
//!    encodings que la ventana pinta. Barata, y basta para rechazar lo que
//!    ni siquiera dice ser una imagen o miente sobre lo que es.
//! 2. [`reencode`]: se DECODIFICA en el host con el crate `image` (Rust
//!    seguro, con límites de tamaño) y se vuelve a codificar. Lo que llega
//!    al `blob:` es un raster hecho aquí; los bytes del guest no salen del
//!    proceso. Una cabecera veraz sobre un flujo comprimido malformado —el
//!    poliglota que pasa la puerta 1— muere en un decodificador de Rust,
//!    no en uno de C con el escritorio detrás.

use std::io::Cursor;

/// El lado mayor que se le pide a un guest, como mucho (espejo de
/// `runtime::THUMB_MAX_EDGE`, aquí para los límites del decodificador).
const MAX_EDGE: u32 = 2048;

/// Memoria que el decodificador del host puede pedir por miniatura: un
/// 2048×2048 RGBA son 16 MiB; el doble deja sitio a las tablas del códec.
const MAX_DECODE_ALLOC: u64 = 32 * 1024 * 1024;

/// Re-codifica `bytes` (ya pasados por [`sniff`]) en el host: decodifica con
/// límites, comprueba que las dimensiones decodificadas son `w`×`h`, y
/// escribe PNG — o JPEG de calidad 85 si el PNG no cabe en `max_bytes` (una
/// foto de 2048 px en PNG son diez megas; el mismo raster en JPEG, uno).
/// Devuelve el mimetype del raster que sale y sus bytes.
///
/// # Errors
/// El mensaje del decodificador o del codificador, para el registro del
/// host; la miniatura entonces no cruza.
pub fn reencode(
    bytes: &[u8],
    w: u32,
    h: u32,
    max_bytes: usize,
) -> Result<(&'static str, Vec<u8>), String> {
    let mut reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| format!("cabecera: {e}"))?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_EDGE);
    limits.max_image_height = Some(MAX_EDGE);
    limits.max_alloc = Some(MAX_DECODE_ALLOC);
    reader.limits(limits);
    let img = reader.decode().map_err(|e| format!("decodificar: {e}"))?;
    if (img.width(), img.height()) != (w, h) {
        return Err(format!(
            "la cabecera decía {w}x{h} y el raster es {}x{}",
            img.width(),
            img.height()
        ));
    }
    let mut png = Cursor::new(Vec::new());
    img.write_to(&mut png, image::ImageFormat::Png)
        .map_err(|e| format!("codificar png: {e}"))?;
    let png = png.into_inner();
    if png.len() <= max_bytes {
        return Ok(("image/png", png));
    }
    let rgb = img.to_rgb8();
    let mut jpeg = Cursor::new(Vec::new());
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 85)
        .encode_image(&rgb)
        .map_err(|e| format!("codificar jpeg: {e}"))?;
    let jpeg = jpeg.into_inner();
    if jpeg.len() > max_bytes {
        return Err(format!(
            "ni en JPEG cabe: {} bytes con un techo de {max_bytes}",
            jpeg.len()
        ));
    }
    Ok(("image/jpeg", jpeg))
}

/// Los encodings que la ventana pinta, con su mimetype.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThumbKind {
    Png,
    Jpeg,
    Webp,
}

impl ThumbKind {
    /// El mimetype que el guest tiene que haber declarado para este raster.
    #[must_use]
    pub const fn mimetype(self) -> &'static str {
        match self {
            ThumbKind::Png => "image/png",
            ThumbKind::Jpeg => "image/jpeg",
            ThumbKind::Webp => "image/webp",
        }
    }
}

/// Lo que la cabecera dice: el encoding por su magia y las dimensiones.
/// `None` si no es ninguno de los tres o la cabecera no llega entera.
#[must_use]
pub fn sniff(bytes: &[u8]) -> Option<(ThumbKind, u32, u32)> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        // IHDR es SIEMPRE el primer chunk: firma (8) + longitud (4) + tipo
        // (4) + ancho (4) + alto (4).
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

/// Recorre los marcadores hasta el primer SOF (`FFC0`–`FFCF`, salvo los
/// que no son frames: `C4` DHT, `C8` JPG, `CC` DAC) y lee alto y ancho.
fn jpeg_dims(bytes: &[u8]) -> Option<(u32, u32)> {
    let mut i = 2usize;
    while i + 4 <= bytes.len() {
        if bytes[i] != 0xFF {
            return None;
        }
        let marker = bytes[i + 1];
        // Relleno `FF FF …` entre marcadores.
        if marker == 0xFF {
            i += 1;
            continue;
        }
        // Sin carga: SOI, EOI, RSTn, TEM.
        if marker == 0xD8 || marker == 0xD9 || (0xD0..=0xD7).contains(&marker) || marker == 0x01 {
            i += 2;
            continue;
        }
        let len = usize::from(u16::from_be_bytes([bytes[i + 2], bytes[i + 3]]));
        if len < 2 {
            return None;
        }
        let es_sof = (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC);
        if es_sof {
            // Longitud (2) + precisión (1) + alto (2) + ancho (2).
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

/// Los tres sabores de WebP: `VP8 ` (con pérdida), `VP8L` (sin pérdida) y
/// `VP8X` (extendido, con el lienzo en la propia cabecera).
fn webp_dims(bytes: &[u8]) -> Option<(u32, u32)> {
    let chunk = &bytes[12..16];
    match chunk {
        b"VP8X" => {
            // Lienzo: 24 bits ancho-1 y 24 bits alto-1, tras 4 bytes de flags.
            if bytes.len() < 30 {
                return None;
            }
            let w = 1 + u32::from_le_bytes([bytes[24], bytes[25], bytes[26], 0]);
            let h = 1 + u32::from_le_bytes([bytes[27], bytes[28], bytes[29], 0]);
            Some((w, h))
        }
        b"VP8L" => {
            // Firma 0x2F y 14 bits de ancho-1, 14 de alto-1.
            if bytes.len() < 25 || bytes[20] != 0x2F {
                return None;
            }
            let b = u32::from_le_bytes([bytes[21], bytes[22], bytes[23], bytes[24]]);
            Some((1 + (b & 0x3FFF), 1 + ((b >> 14) & 0x3FFF)))
        }
        b"VP8 " => {
            // Frame tag (3) + código de inicio 9d 01 2a (3) + ancho (2) + alto
            // (2), los 14 bits bajos de cada uno.
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
    reason = "rásteres de juguete: los píxeles se generan con aritmética modular"
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
        // SOI, APP0 de 16 bytes, DHT (no es frame), SOF0 320x200.
        let mut v = vec![0xFF, 0xD8];
        v.extend_from_slice(&[0xFF, 0xE0, 0x00, 0x10]);
        v.extend_from_slice(&[0u8; 14]);
        v.extend_from_slice(&[0xFF, 0xC4, 0x00, 0x04, 0, 0]);
        v.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x11, 8]);
        v.extend_from_slice(&200u16.to_be_bytes());
        v.extend_from_slice(&320u16.to_be_bytes());
        v.extend_from_slice(&[3, 1, 0x22, 0, 2, 0x11, 1, 3, 0x11, 1]);
        assert_eq!(sniff(&v), Some((ThumbKind::Jpeg, 320, 200)));
        assert_eq!(sniff(&[0xFF, 0xD8, 0xFF, 0xE0]), None, "sin SOF");
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

    /// Un PNG de verdad, hecho con el mismo crate: la puerta 2 tiene que
    /// DECODIFICAR, y una cabecera sola no basta.
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
        let (mime, out) = reencode(&bytes, 64, 32, 4 * 1024 * 1024).expect("re-codifica");
        assert_eq!(mime, "image/png");
        assert!(out.starts_with(b"\x89PNG"));
        assert_eq!(sniff(&out), Some((ThumbKind::Png, 64, 32)));
        // Cabecera veraz sobre un flujo roto: pasa `sniff`, muere aquí.
        let mut roto = bytes.clone();
        for b in roto.iter_mut().skip(40) {
            *b = 0xAA;
        }
        assert_eq!(sniff(&roto).map(|(k, _, _)| k), Some(ThumbKind::Png));
        assert!(reencode(&roto, 64, 32, 4 * 1024 * 1024).is_err());
        // Dimensiones que no son las decodificadas.
        assert!(reencode(&bytes, 32, 64, 4 * 1024 * 1024).is_err());
    }

    #[test]
    fn reencode_falls_back_to_jpeg_when_png_does_not_fit() {
        // Ruido: PNG no comprime, y un techo pequeño lo echa a JPEG.
        let img = image::RgbaImage::from_fn(256, 256, |x, y| {
            let v = (x.wrapping_mul(2_654_435_761) ^ y.wrapping_mul(40_503)) as u8;
            image::Rgba([v, v.wrapping_mul(3), v.wrapping_mul(7), 255])
        });
        let mut out = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut out, image::ImageFormat::Png)
            .expect("png");
        let (mime, bytes) =
            reencode(&out.into_inner(), 256, 256, 120 * 1024).expect("cabe en jpeg");
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
