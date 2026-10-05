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

/// One icon to place: which, in what colour, over which 2×2 cells.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RailIcon {
    /// The panel kind, whose shared SVG is drawn.
    pub kind: String,
    /// The stroke colour: the state's role, from the theme.
    pub rgb: [u8; 3],
    /// The 2×2 cells `draw_rail` left blank for it.
    pub rect: ratatui::layout::Rect,
}

/// The first kitty image id of the column's range. The viewer's ids count
/// up from 1 (`viewer_open`), so the two never meet; one id per slot.
pub const ID_BASE: u32 = 0x4E52_0000;
/// How many slots the range holds — far more than buttons exist.
pub const MAX_SLOTS: u32 = 64;

/// The image id of slot `i`.
#[must_use]
pub fn id_for(i: usize) -> u32 {
    ID_BASE + u32::try_from(i).unwrap_or(MAX_SLOTS - 1).min(MAX_SLOTS - 1)
}

/// What is on the terminal right now, slot by slot. PROCESS state, like
/// `kitty_graphics::PLACED`: the exit paths that erase have no `App`.
static PLACED: std::sync::Mutex<Vec<Option<RailIcon>>> = std::sync::Mutex::new(Vec::new());

fn placed() -> std::sync::MutexGuard<'static, Vec<Option<RailIcon>>> {
    PLACED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Brings the terminal to `want`: a slot that changed is deleted and
/// placed again, one no longer wanted is deleted, one unchanged is left
/// alone — a still frame writes nothing. A failed write forgets that slot
/// so the next frame retries it.
pub fn sync(out: &mut impl std::io::Write, want: &[RailIcon]) {
    use crate::kitty_graphics::{escape_delete, escape_place};
    let mut placed = placed();
    let len = placed.len().max(want.len());
    placed.resize(len, None);
    for (i, slot) in placed.iter_mut().enumerate() {
        let wants = want.get(i);
        if slot.as_ref() == wants {
            continue;
        }
        let id = id_for(i);
        let mut esc = String::new();
        if slot.take().is_some() {
            esc.push_str(&escape_delete(id));
        }
        let png = wants.and_then(|w| png(&w.kind, w.rgb).map(|p| (w, p)));
        let result = out.write_all(esc.as_bytes()).and_then(|()| match &png {
            Some((w, bytes)) => {
                crossterm::queue!(out, crossterm::cursor::MoveTo(w.rect.x, w.rect.y))?;
                out.write_all(escape_place(id, bytes, w.rect, None).as_bytes())
            }
            None => Ok(()),
        });
        match result {
            Ok(()) => *slot = png.map(|(w, _)| w.clone()),
            Err(e) => {
                // An APC cut short swallows whatever is painted after it.
                let _ = out.write_all(b"\x1b\\");
                tracing::debug!(error = %e, id, "could not place a panel icon");
            }
        }
    }
    placed.truncate(want.len());
    let _ = out.flush();
}

/// Takes every column icon off the terminal and forgets them: the exit,
/// suspend and panic paths, next to `kitty_graphics::delete_placed`.
pub fn delete_all(out: &mut impl std::io::Write) {
    let mut placed = placed();
    if placed.iter().all(Option::is_none) {
        placed.clear();
        return;
    }
    let esc = format!(
        "\x1b_Ga=d,d=R,x={},y={},q=2\x1b\\",
        ID_BASE,
        ID_BASE + MAX_SLOTS - 1
    );
    if let Err(e) = out.write_all(esc.as_bytes()).and_then(|()| out.flush()) {
        tracing::debug!(error = %e, "could not erase the panel icons");
    }
    placed.clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use norte_config::Images;

    fn icon(kind: &str, rgb: [u8; 3], x: u16, y: u16) -> RailIcon {
        RailIcon {
            kind: kind.to_owned(),
            rgb,
            rect: ratatui::layout::Rect::new(x, y, 2, 2),
        }
    }

    /// Runs `f` against a fresh writer, after forgetting whatever an
    /// earlier step placed, and hands back what it wrote.
    fn written(f: impl FnOnce(&mut Vec<u8>)) -> String {
        let mut out = Vec::new();
        f(&mut out);
        String::from_utf8(out).expect("escapes are ascii")
    }

    #[test]
    fn the_first_sync_places_and_the_second_writes_nothing() {
        let _ = written(delete_all);
        let want = [icon("places", [1, 2, 3], 1, 2)];
        let first = written(|o| sync(o, &want));
        assert!(first.contains(&format!("a=T,i={}", id_for(0))), "{first:?}");
        assert!(first.contains("c=2,r=2"), "{first:?}");
        assert_eq!(written(|o| sync(o, &want)), "", "nothing changed");
    }

    #[test]
    fn a_new_colour_replaces_the_old_image() {
        let _ = written(delete_all);
        let _ = written(|o| sync(o, &[icon("places", [1, 2, 3], 1, 2)]));
        let again = written(|o| sync(o, &[icon("places", [9, 9, 9], 1, 2)]));
        let del = again
            .find(&format!("d=I,i={}", id_for(0)))
            .expect("deleted");
        let put = again.find(&format!("a=T,i={}", id_for(0))).expect("placed");
        assert!(del < put, "delete first: {again:?}");
    }

    #[test]
    fn shrinking_drops_to_glyphs_and_erases() {
        let _ = written(delete_all);
        let _ = written(|o| sync(o, &[icon("places", [1, 2, 3], 1, 2)]));
        let gone = written(|o| sync(o, &[]));
        assert!(gone.contains(&format!("d=I,i={}", id_for(0))), "{gone:?}");
        assert_eq!(written(|o| sync(o, &[])), "");
    }

    /// The exit paths call it on EVERY terminal: one that never got an icon
    /// must not receive an APC either.
    #[test]
    fn delete_all_with_nothing_placed_writes_nothing() {
        let _ = written(delete_all);
        assert_eq!(written(delete_all), "");
    }

    #[test]
    fn delete_all_clears_the_range_and_the_state() {
        let want = [icon("places", [1, 2, 3], 1, 2)];
        let _ = written(|o| sync(o, &want));
        let all = written(delete_all);
        assert!(
            all.contains(&format!("d=R,x={},y={}", ID_BASE, ID_BASE + MAX_SLOTS - 1)),
            "{all:?}"
        );
        assert!(written(|o| sync(o, &want)).contains("a=T"), "placed again");
    }

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
