//! Lo que el host comprueba de una miniatura ANTES de que cruce (ADR 0107,
//! decisión 3): un guest devuelve bytes de imagen, y esos bytes acaban en
//! un `blob:` de la webview. Aquí se lee solo la cabecera —magia y
//! dimensiones— de los tres encodings que la ventana pinta, y nada más: un
//! decodificador entero en el host sería justo la superficie que el sandbox
//! existe para no tener.

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

    #[test]
    fn anything_else_is_not_a_thumbnail() {
        assert_eq!(sniff(b"GIF89a"), None);
        assert_eq!(sniff(b""), None);
        assert_eq!(sniff(b"BM"), None);
    }
}
