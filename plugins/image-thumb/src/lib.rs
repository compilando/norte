//! `org.norte.image-thumb`: a downscaled raster of an image file.
//!
//! Implements the `norte-thumbnail` world (ADR 0107): the host hands over
//! the file's bytes and the longest edge allowed, and gets back an encoded
//! image with its dimensions. The whole decision lives in [`thumbnail`], a
//! pure function over bytes with its own tests; the WIT glue below only
//! exists when compiled AS a component, so the host-side tests build the
//! same crate without it.

use std::io::Cursor;

use image::imageops::FilterType;
use image::{DynamicImage, ImageFormat, ImageReader};

/// The encoding of the thumbnail (`[config] format`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// Small: a tenth the size of PNG for a photo.
    Jpeg,
    /// Exact, with transparency.
    Png,
}

impl Format {
    /// The mimetype the host expects back for this encoding.
    #[must_use]
    pub const fn mimetype(self) -> &'static str {
        match self {
            Format::Jpeg => "image/jpeg",
            Format::Png => "image/png",
        }
    }
}

/// The two settings, already parsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Params {
    pub format: Format,
    /// JPEG quality, `30..=95`.
    pub quality: u8,
}

impl Default for Params {
    fn default() -> Self {
        Self {
            format: Format::Jpeg,
            quality: 80,
        }
    }
}

impl Params {
    /// From the `[config]` values as the host hands them over (strings,
    /// `None` when absent). Anything unreadable is the default.
    pub fn parse(format: Option<&str>, quality: Option<&str>) -> Self {
        let d = Self::default();
        Self {
            format: match format {
                Some("png") => Format::Png,
                Some("jpeg") => Format::Jpeg,
                _ => d.format,
            },
            quality: quality
                .and_then(|q| q.trim().parse::<u8>().ok())
                .map_or(d.quality, |q| q.clamp(30, 95)),
        }
    }
}

/// A finished thumbnail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thumb {
    pub mimetype: &'static str,
    pub bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// The thumbnail of `content`: decoded by its own header (the mimetype the
/// host guessed from the name is not trusted), scaled down so that neither
/// edge exceeds `max_edge` — never scaled UP — and re-encoded.
///
/// # Errors
/// A format the crate cannot decode, a truncated file, or an encoder
/// failure: the message is for the host's log, and the viewer keeps what it
/// had.
pub fn thumbnail(content: &[u8], max_edge: u32, p: &Params) -> Result<Thumb, String> {
    let max_edge = max_edge.max(1);
    let mut reader = ImageReader::new(Cursor::new(content))
        .with_guessed_format()
        .map_err(|e| format!("cannot read header: {e}"))?;
    // The bytes come from any provider — untrusted. Refuse from the header
    // what would not fit the sandbox anyway (the store has 64 MiB), instead
    // of paying the allocations up to that ceiling and trapping.
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(16_384);
    limits.max_image_height = Some(16_384);
    limits.max_alloc = Some(40 * 1024 * 1024);
    reader.limits(limits);
    let decoded = reader.decode().map_err(|e| format!("cannot decode: {e}"))?;
    let (w, h) = (decoded.width(), decoded.height());
    let small: DynamicImage = if w <= max_edge && h <= max_edge {
        decoded
    } else {
        // `thumbnail` is the fast box filter; `resize` with Triangle is what
        // keeps a photo from looking like a mosaic at these sizes.
        decoded.resize(max_edge, max_edge, FilterType::Triangle)
    };
    let (width, height) = (small.width(), small.height());
    let mut out = Cursor::new(Vec::new());
    match p.format {
        Format::Png => small
            .write_to(&mut out, ImageFormat::Png)
            .map_err(|e| format!("cannot encode png: {e}"))?,
        Format::Jpeg => {
            // JPEG has no alpha: flatten to RGB first or the encoder refuses.
            let rgb = small.to_rgb8();
            let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, p.quality);
            enc.encode_image(&rgb)
                .map_err(|e| format!("cannot encode jpeg: {e}"))?;
        }
    }
    Ok(Thumb {
        mimetype: p.format.mimetype(),
        bytes: out.into_inner(),
        width,
        height,
    })
}

#[cfg(target_arch = "wasm32")]
mod guest {
    wit_bindgen::generate!({
        world: "norte:thumbnail/norte-thumbnail",
        path: "wit",
        generate_all,
    });

    use exports::norte::thumbnail::thumbnail::{Guest as ThumbnailGuest, Thumb, ThumbInput};
    use norte::host::host_config;

    use crate::{Params, thumbnail};

    struct ImageThumb;

    impl ThumbnailGuest for ImageThumb {
        fn render(input: ThumbInput) -> Result<Thumb, String> {
            let params = Params::parse(
                host_config::get("format").as_deref(),
                host_config::get("quality").as_deref(),
            );
            let t = thumbnail(&input.content, input.max_edge, &params)?;
            Ok(Thumb {
                mimetype: t.mimetype.to_owned(),
                bytes: t.bytes,
                width: t.width,
                height: t.height,
            })
        }
    }

    export!(ImageThumb);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A PNG of `w`×`h` with a gradient, encoded with the same crate.
    fn png(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbaImage::from_fn(w, h, |x, y| {
            image::Rgba([(x % 256) as u8, (y % 256) as u8, 128, 255])
        });
        let mut out = Cursor::new(Vec::new());
        DynamicImage::ImageRgba8(img)
            .write_to(&mut out, ImageFormat::Png)
            .expect("png");
        out.into_inner()
    }

    #[test]
    fn a_big_image_is_scaled_to_the_edge_and_keeps_its_ratio() {
        let t = thumbnail(&png(800, 400), 200, &Params::default()).expect("thumb");
        assert_eq!((t.width, t.height), (200, 100));
        assert_eq!(t.mimetype, "image/jpeg");
        assert!(t.bytes.starts_with(&[0xFF, 0xD8]), "a JPEG");
        assert!(t.bytes.len() < 20_000, "small: {}", t.bytes.len());
    }

    #[test]
    fn a_small_image_is_not_scaled_up() {
        let t = thumbnail(&png(50, 30), 200, &Params::default()).expect("thumb");
        assert_eq!((t.width, t.height), (50, 30));
    }

    #[test]
    fn png_keeps_the_encoding_and_a_bad_file_is_an_error() {
        let p = Params {
            format: Format::Png,
            quality: 80,
        };
        let t = thumbnail(&png(300, 300), 100, &p).expect("thumb");
        assert_eq!((t.width, t.height), (100, 100));
        assert_eq!(t.mimetype, "image/png");
        assert!(t.bytes.starts_with(b"\x89PNG"));
        assert!(thumbnail(b"not an image at all", 100, &p).is_err());
        assert!(
            thumbnail(&png(300, 300)[..40], 100, &p).is_err(),
            "truncated"
        );
    }

    #[test]
    fn the_settings_parse_and_clamp() {
        let p = Params::parse(Some("png"), Some("200"));
        assert_eq!(p.format, Format::Png);
        assert_eq!(p.quality, 95);
        assert_eq!(Params::parse(None, Some("x")), Params::default());
        assert_eq!(Params::parse(Some("jpeg"), Some("10")).quality, 30);
    }
}
