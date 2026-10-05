//! The panel column's big icons (spec 2026-10-05): which terminal can
//! paint them, and how.
//!
//! The column is either today's one-cell glyphs or, on a terminal that
//! can paint graphics, the window's own icons at 2×2 cells. Which one is
//! decided ONCE, by probing, never by `TERM` — a terminal that cannot answer
//! the probe (tmux without passthrough) keeps the glyphs.

/// What the panel column draws its icons with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RailBackend {
    /// Kitty's graphics protocol: the shared SVG rasterised, 2×2 cells.
    KittyGraphics,
    /// One cell per icon: Unicode, Nerd Font or the letter. Every terminal.
    #[default]
    Glyph,
}

/// The backend for this terminal: `[ui] images` first — `off` and `blocks`
/// are "no pixels here", and `kitty` trusts the reader over the probe, as
/// the viewer does — then the probe's answer.
#[must_use]
pub fn choose(images: norte_config::Images, kitty: bool) -> RailBackend {
    use norte_config::Images;
    match images {
        Images::Kitty => RailBackend::KittyGraphics,
        Images::Auto if kitty => RailBackend::KittyGraphics,
        Images::Auto | Images::Off | Images::Blocks => RailBackend::Glyph,
    }
}

/// This frame's backend: derived, not stored, so a config reload or the
/// startup probe needs nobody to remember to update it.
#[must_use]
pub fn backend(app: &crate::app::App) -> RailBackend {
    choose(app.chrome.images(), crate::kitty_graphics::supported())
}

/// The side, in pixels, an icon is rasterised at. Kitty scales the raster
/// to the 2×2 cells it is given, so this is a quality knob, not a size:
/// 64 px stays sharp on high-density cells and is ~1 KB of PNG.
pub const RASTER_PX: u32 = 64;

type Cache = std::collections::HashMap<(String, [u8; 3]), std::sync::Arc<Vec<u8>>>;

/// `kind`'s shared icon (`panelbar::icon_svg`) as a PNG stroked in `rgb`,
/// cached by both: a frame asks for the same few every time. `None` for a
/// kind without an SVG, or if rendering fails (logged; the slot stays
/// blank for that frame).
#[must_use]
pub fn png(kind: &str, rgb: [u8; 3]) -> Option<std::sync::Arc<Vec<u8>>> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<Cache>> = std::sync::OnceLock::new();
    let svg = norte_frontend::panelbar::icon_svg(kind)?;
    let cache = CACHE.get_or_init(Default::default);
    let key = (kind.to_owned(), rgb);
    if let Some(hit) = cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&key)
    {
        return Some(hit.clone());
    }
    let bytes = std::sync::Arc::new(rasterise(svg, rgb).or_else(|| {
        tracing::debug!(kind, "could not rasterise the panel icon");
        None
    })?);
    cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(key, bytes.clone());
    Some(bytes)
}

fn rasterise(svg: &str, [r, g, b]: [u8; 3]) -> Option<Vec<u8>> {
    use resvg::{tiny_skia, usvg};
    let src = svg.replace("currentColor", &format!("#{r:02x}{g:02x}{b:02x}"));
    let tree = usvg::Tree::from_str(&src, &usvg::Options::default()).ok()?;
    let mut pixmap = tiny_skia::Pixmap::new(RASTER_PX, RASTER_PX)?;
    // `as f32` on 64 and 24 is exact.
    #[allow(clippy::cast_precision_loss)]
    let scale = RASTER_PX as f32 / tree.size().width();
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    pixmap.encode_png().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use norte_config::Images;

    #[test]
    fn the_star_rasterises_in_the_requested_colour() {
        let bytes = png("places", [0xff, 0x00, 0x00]).expect("places has an svg");
        let pix = resvg::tiny_skia::Pixmap::decode_png(&bytes).expect("valid png");
        assert_eq!((pix.width(), pix.height()), (RASTER_PX, RASTER_PX));
        // ON the star's top stroke: (12, 3.5) of the 24 grid.
        let p = pix
            .pixel(RASTER_PX / 2, RASTER_PX * 35 / 240 + 1)
            .expect("inside");
        assert!(p.red() > 200 && p.green() < 40 && p.alpha() > 200, "{p:?}");
        // And the middle of the star is empty: strokes, not a fill.
        let c = pix.pixel(RASTER_PX / 2, RASTER_PX / 2).expect("inside");
        assert_eq!(c.alpha(), 0, "{c:?}");
        assert!(png("plugin:x:y", [0, 0, 0]).is_none());
    }

    #[test]
    fn choose_follows_the_probe_and_the_images_switch() {
        assert_eq!(choose(Images::Auto, true), RailBackend::KittyGraphics);
        assert_eq!(choose(Images::Kitty, false), RailBackend::KittyGraphics);
        assert_eq!(choose(Images::Auto, false), RailBackend::Glyph);
        assert_eq!(choose(Images::Off, true), RailBackend::Glyph);
        assert_eq!(choose(Images::Blocks, true), RailBackend::Glyph);
    }
}
