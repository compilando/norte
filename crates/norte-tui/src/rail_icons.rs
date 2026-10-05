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

/// The canvas, in pixels, for a 2×2-cell slot whose cells measure
/// `cell_px` (`crossterm::terminal::window_size`; `(0, 0)` when the
/// terminal does not say, taken as 1:2). Kitty STRETCHES a raster to the
/// cells it is given, so the canvas must have the slot's proportions; it is
/// scaled up until its short side is at least 64 px, so the strokes stay
/// sharp on dense cells.
#[must_use]
pub fn canvas_for(cell_px: (u16, u16)) -> (u32, u32) {
    let (cw, ch) = match cell_px {
        (0, _) | (_, 0) => (10, 20),
        (w, h) => (u32::from(w), u32::from(h)),
    };
    let (w, h) = (2 * cw, 2 * ch);
    let k = 64_u32.div_ceil(w.min(h)).clamp(1, 8);
    (w * k, h * k)
}

/// One cell's size in pixels, as the terminal reports it (`TIOCGWINSZ`);
/// `(0, 0)` when it does not, which [`canvas_for`] takes as 1:2.
#[must_use]
pub fn cell_px() -> (u16, u16) {
    crossterm::terminal::window_size().map_or((0, 0), |s| {
        if s.columns == 0 || s.rows == 0 {
            (0, 0)
        } else {
            (s.width / s.columns, s.height / s.rows)
        }
    })
}

type Cache = std::collections::HashMap<(String, [u8; 3], (u32, u32)), std::sync::Arc<Vec<u8>>>;

/// `kind`'s shared icon (`panelbar::icon_svg`) as a PNG stroked in `rgb`
/// on a `canvas` (`canvas_for`), cached by the three: a frame asks for the
/// same few every time. `None` for a kind without an SVG, or if rendering
/// fails (logged; the slot stays blank for that frame).
#[must_use]
pub fn png(kind: &str, rgb: [u8; 3], canvas: (u32, u32)) -> Option<std::sync::Arc<Vec<u8>>> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<Cache>> = std::sync::OnceLock::new();
    let svg = norte_frontend::panelbar::icon_svg(kind)?;
    let cache = CACHE.get_or_init(Default::default);
    let key = (kind.to_owned(), rgb, canvas);
    if let Some(hit) = cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&key)
    {
        return Some(hit.clone());
    }
    let bytes = std::sync::Arc::new(rasterise(svg, rgb, canvas).or_else(|| {
        tracing::debug!(kind, "could not rasterise the panel icon");
        None
    })?);
    cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(key, bytes.clone());
    Some(bytes)
}

/// The icon as a square of side `min(w, h)` centred on a `w`×`h` canvas.
fn rasterise(svg: &str, rgb: [u8; 3], (w, h): (u32, u32)) -> Option<Vec<u8>> {
    use resvg::{tiny_skia, usvg};
    let [red, green, blue] = rgb;
    let src = svg.replace("currentColor", &format!("#{red:02x}{green:02x}{blue:02x}"));
    let tree = usvg::Tree::from_str(&src, &usvg::Options::default()).ok()?;
    let mut pixmap = tiny_skia::Pixmap::new(w, h)?;
    // Canvas sides are at most a few hundred pixels: exact in `f32`.
    #[allow(clippy::cast_precision_loss)]
    let (wf, hf) = (w as f32, h as f32);
    let side = wf.min(hf);
    let scale = side / tree.size().width();
    let transform = tiny_skia::Transform::from_scale(scale, scale)
        .post_translate((wf - side) / 2.0, (hf - side) / 2.0);
    resvg::render(&tree, transform, &mut pixmap.as_mut());
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
    /// The raster's pixels, with the slot's proportions (`canvas_for`).
    pub canvas: (u32, u32),
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
    // Rasterised BEFORE taking the lock: a panic inside resvg must not find
    // the panic hook waiting on a lock its own thread holds
    // (`delete_all`).
    let pngs: Vec<_> = want.iter().map(|w| png(&w.kind, w.rgb, w.canvas)).collect();
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
        let png = wants.zip(pngs.get(i).cloned().flatten());
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

/// Forgets what is placed WITHOUT writing: the screen was cleared (a resize
/// redraws with ED 2, and kitty drops every placement with it), so the next
/// [`sync`] places everything again.
pub fn forget() {
    placed().clear();
}

/// Takes every column icon off the terminal and forgets them: the exit,
/// suspend and panic paths, next to `kitty_graphics::delete_placed`.
///
/// `try_lock`: the panic hook runs on the panicking thread, which may be
/// inside [`sync`] holding the lock. Then it writes nothing — leaving the
/// alternate screen comes next and matters more than the icons.
pub fn delete_all(out: &mut impl std::io::Write) {
    let mut placed = match PLACED.try_lock() {
        Ok(guard) => guard,
        Err(std::sync::TryLockError::Poisoned(p)) => p.into_inner(),
        Err(std::sync::TryLockError::WouldBlock) => return,
    };
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
            canvas: (64, 128),
        }
    }

    /// Kitty stretches a raster to the `c`×`r` cells it is given, and cells
    /// are about twice as tall as wide: a square canvas came out as a tall
    /// oval. The canvas has the slot's proportions and the icon sits square
    /// in its middle.
    #[test]
    fn a_tall_slot_keeps_the_icon_square() {
        let bytes = png("places", [0xff, 0, 0], (40, 80)).expect("svg");
        let pix = resvg::tiny_skia::Pixmap::decode_png(&bytes).expect("png");
        assert_eq!((pix.width(), pix.height()), (40, 80));
        // The square is rows 20..60: nothing above or below it.
        for y in (0..20).chain(60..80) {
            for x in 0..40 {
                assert_eq!(pix.pixel(x, y).expect("in").alpha(), 0, "({x},{y})");
            }
        }
        // The star's top stroke, (12, 3.5) of 24, inside the square.
        let p = pix.pixel(20, 20 + 40 * 35 / 240 + 1).expect("in");
        assert!(p.red() > 200 && p.alpha() > 200, "{p:?}");
    }

    #[test]
    fn the_canvas_follows_the_cell_and_stays_sharp() {
        // 9×19 px cells: a 18×38 slot, scaled ×4 so the short side is ≥ 64.
        assert_eq!(canvas_for((9, 19)), (72, 152));
        // A terminal that reports no pixels: assume 1:2.
        assert_eq!(canvas_for((0, 0)), (80, 160));
        // Big cells need no scaling.
        assert_eq!(canvas_for((40, 80)), (80, 160));
    }

    /// A resize clears the screen (ED 2) and kitty drops every placement
    /// with it: what this module believes is placed is no longer true.
    #[test]
    fn forget_makes_the_next_sync_place_again() {
        let _ = written(delete_all);
        let want = [icon("places", [1, 2, 3], 1, 2)];
        let _ = written(|o| sync(o, &want));
        forget();
        assert!(written(|o| sync(o, &want)).contains("a=T"));
    }

    /// The panic hook runs on the panicking thread, which may hold the lock
    /// (a panic inside `sync`): it must return, writing nothing, instead of
    /// deadlocking before the terminal leaves the alternate screen.
    #[test]
    fn delete_all_under_a_held_lock_returns() {
        let _ = written(delete_all);
        let _ = written(|o| sync(o, &[icon("places", [1, 2, 3], 1, 2)]));
        let guard = PLACED.lock().expect("not poisoned");
        assert_eq!(written(delete_all), "");
        drop(guard);
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
        const S: u32 = 64;
        let bytes = png("places", [0xff, 0x00, 0x00], (S, S)).expect("places has an svg");
        let pix = resvg::tiny_skia::Pixmap::decode_png(&bytes).expect("valid png");
        assert_eq!((pix.width(), pix.height()), (S, S));
        // ON the star's top stroke: (12, 3.5) of the 24 grid.
        let p = pix.pixel(S / 2, S * 35 / 240 + 1).expect("inside");
        assert!(p.red() > 200 && p.green() < 40 && p.alpha() > 200, "{p:?}");
        // And the middle of the star is empty: strokes, not a fill.
        let c = pix.pixel(S / 2, S / 2).expect("inside");
        assert_eq!(c.alpha(), 0, "{c:?}");
        assert!(png("plugin:x:y", [0, 0, 0], (S, S)).is_none());
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
