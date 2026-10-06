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
    /// DEC sixel (F2): the same raster, painted INTO the cells — ratatui
    /// repainting a cell erases it, and there is nothing to delete.
    Sixel,
    /// One cell per icon: Unicode, Nerd Font or the letter. Every terminal.
    #[default]
    Glyph,
}

/// The backend for this terminal: `[ui] images` first — `off` and `blocks`
/// are "no pixels here", and `kitty` trusts the reader over the probe, as
/// the viewer does — then the probe's answers, kitty before sixel. Sixel
/// does not scale, so it needs the cell size in pixels (`cell_px`, only
/// measured when sixel is the candidate: it is an ioctl); a terminal that
/// does not report it keeps the glyphs.
#[must_use]
pub fn choose(
    images: norte_config::Images,
    kitty: bool,
    sixel: bool,
    cell_px: impl FnOnce() -> (u16, u16),
) -> RailBackend {
    use norte_config::Images;
    match images {
        Images::Kitty => RailBackend::KittyGraphics,
        Images::Auto if kitty => RailBackend::KittyGraphics,
        Images::Auto if sixel && matches!(cell_px(), (w, h) if w > 0 && h > 0) => {
            RailBackend::Sixel
        }
        Images::Auto | Images::Off | Images::Blocks => RailBackend::Glyph,
    }
}

/// This frame's backend: derived, not stored, so a config reload or the
/// startup probe needs nobody to remember to update it.
#[must_use]
pub fn backend(app: &crate::app::App) -> RailBackend {
    choose(
        app.chrome.images(),
        crate::kitty_graphics::supported(),
        crate::kitty_graphics::sixel_supported(),
        cell_px,
    )
}

/// A sixel slot's canvas: the 2×2 cells' pixels exactly — sixel paints
/// pixel for pixel, with no scaling to fit.
#[must_use]
pub fn canvas_exact((cw, ch): (u16, u16)) -> (u32, u32) {
    (2 * u32::from(cw), 2 * u32::from(ch))
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
    // What the terminal said at startup (`CSI 16 t`) first: exact, and the
    // only source on Windows, where crossterm cannot ask.
    if let Some(px) = crate::kitty_graphics::probed_cell_px() {
        return px;
    }
    crossterm::terminal::window_size().map_or((0, 0), |s| {
        if s.columns == 0 || s.rows == 0 {
            (0, 0)
        } else {
            (s.width / s.columns, s.height / s.rows)
        }
    })
}

type Pixmap = resvg::tiny_skia::Pixmap;
type Cache =
    std::collections::HashMap<(String, [u8; 3], (u32, u32)), Option<std::sync::Arc<Pixmap>>>;

/// How many rasterisations ran: a test seam for "a failure is not retried".
#[cfg(test)]
static RASTERISED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// `kind`'s shared icon (`panelbar::icon_svg`) stroked in `rgb` on a
/// `canvas`, cached by the three — a failure too, logged once: a frame asks
/// for the same few every time. `None` for a kind without an SVG, or if
/// rendering fails (the slot stays blank).
fn raster(kind: &str, rgb: [u8; 3], canvas: (u32, u32)) -> Option<std::sync::Arc<Pixmap>> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<Cache>> = std::sync::OnceLock::new();
    let svg = norte_frontend::panelbar::icon_svg(kind)?;
    let cache = CACHE.get_or_init(Default::default);
    let key = (kind.to_owned(), rgb, canvas);
    if let Some(hit) = cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&key)
    {
        return hit.clone();
    }
    #[cfg(test)]
    RASTERISED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let pixmap = rasterise(svg, rgb, canvas).map(std::sync::Arc::new);
    if pixmap.is_none() {
        tracing::debug!(kind, ?canvas, "could not rasterise the panel icon");
    }
    cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(key, pixmap.clone());
    pixmap
}

/// `kind`'s icon stroked in `rgb` on a `canvas`, as the PNG kitty takes.
#[must_use]
pub fn png(kind: &str, rgb: [u8; 3], canvas: (u32, u32)) -> Option<Vec<u8>> {
    raster(kind, rgb, canvas)?.encode_png().ok()
}

/// How many payloads were built: a test seam for "a still frame builds
/// nothing".
#[cfg(test)]
static BUILT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// What a slot sends: its escape, built outside the placed lock.
fn payload(id: u32, icon: &RailIcon) -> Option<String> {
    #[cfg(test)]
    BUILT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let pix = raster(&icon.kind, icon.rgb, icon.canvas)?;
    match icon.backend {
        RailBackend::KittyGraphics => {
            let png = pix.encode_png().ok()?;
            Some(crate::kitty_graphics::escape_place(
                id, &png, icon.rect, None,
            ))
        }
        RailBackend::Sixel => {
            let alpha: Vec<u8> = pix.pixels().iter().map(|p| p.alpha()).collect();
            Some(crate::sixel::encode(
                &alpha,
                pix.width(),
                pix.height(),
                icon.rgb,
                icon.bg,
            ))
        }
        RailBackend::Glyph => None,
    }
}

/// The icon as a square of side `min(w, h)` centred on a `w`×`h` canvas.
fn rasterise(svg: &str, rgb: [u8; 3], (w, h): (u32, u32)) -> Option<Pixmap> {
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
    Some(pixmap)
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
    /// The raster's pixels: the slot's proportions for kitty
    /// (`canvas_for`), its exact pixels for sixel (`canvas_exact`).
    pub canvas: (u32, u32),
    /// What paints it.
    pub backend: RailBackend,
    /// The rail's background, as shown: what sixel blends the antialiased
    /// edge toward.
    pub bg: [u8; 3],
    /// Which slot of the column it fills: its image id (`id_for`), so a
    /// slot with no SVG ahead of it does not shift the others.
    pub slot: usize,
}

impl RailIcon {
    /// Whether `other` is the same IMAGE — only placed elsewhere, or not at
    /// all: a hidden kitty image is then shown again without its bytes.
    fn same_image(&self, other: &Self) -> bool {
        self.kind == other.kind
            && self.rgb == other.rgb
            && self.canvas == other.canvas
            && self.backend == other.backend
            && self.bg == other.bg
    }
}

/// A slot's image on the terminal: shown, or a kitty image hidden under an
/// overlay with its bytes kept (`d=i`).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Placed {
    icon: RailIcon,
    shown: bool,
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
static PLACED: std::sync::Mutex<Vec<Option<Placed>>> = std::sync::Mutex::new(Vec::new());

fn placed() -> std::sync::MutexGuard<'static, Vec<Option<Placed>>> {
    PLACED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Brings the terminal to `want` (one icon per column slot, by
/// `RailIcon::slot`): unchanged slots are left alone — a still frame
/// builds nothing and writes nothing.
///
/// - A kitty icon no longer wanted is HIDDEN with its bytes kept (`d=i`),
///   and shown again by re-placing it (`a=p`) when the same image comes
///   back: closing a menu does not re-send every PNG. A different image
///   deletes the old one (`d=I`) and transmits.
/// - A sixel image is cells, with no escape to remove it: its cells are
///   repainted from `frame`, the one just drawn, since ratatui does not
///   rewrite cells it believes unchanged — ALWAYS, even under a new image
///   in the same cells, whose empty pixels are transparent.
///
/// Everything comes down before anything goes up: an icon that moved one
/// row shares a row with where it was, and a later repaint would erase
/// half of it. The cursor is saved and restored around it all. A failed
/// write keeps what was believed on screen, so the exit can still erase it.
pub fn sync<B>(out: &mut B, want: &[RailIcon], frame: Option<&ratatui::buffer::Buffer>)
where
    B: ratatui::backend::Backend<Error = std::io::Error> + std::io::Write,
{
    use crate::kitty_graphics::{escape_delete, escape_hide, escape_reposition};
    // Decided under the lock and nothing else: building an image runs
    // resvg, and a panic there must not find the panic hook waiting on a
    // lock its own thread holds (`delete_all`).
    let before = placed().clone();
    let len = want
        .iter()
        .map(|w| w.slot + 1)
        .max()
        .unwrap_or(0)
        .max(before.len());
    let mut wanted: Vec<Option<&RailIcon>> = vec![None; len];
    for w in want {
        wanted[w.slot] = Some(w);
    }
    let mut after = before.clone();
    after.resize(len, None);
    let (mut down, mut repaint, mut up) = (String::new(), Vec::new(), Vec::new());
    for (i, w) in wanted.iter().enumerate() {
        let had = before.get(i).cloned().flatten();
        let id = id_for(i);
        match (had, *w) {
            (None, None) => {}
            (Some(p), None) if !p.shown => {}
            (Some(p), None) => {
                if p.icon.backend == RailBackend::KittyGraphics {
                    down.push_str(&escape_hide(id));
                    after[i] = Some(Placed { shown: false, ..p });
                } else {
                    repaint.push(p.icon.rect);
                    after[i] = None;
                }
            }
            (Some(p), Some(w)) if p.shown && p.icon == *w => {}
            (Some(p), Some(w))
                if p.icon.backend == RailBackend::KittyGraphics && p.icon.same_image(w) =>
            {
                up.push((w, escape_reposition(id, w.rect, None)));
                after[i] = Some(Placed {
                    icon: w.clone(),
                    shown: true,
                });
            }
            (had, Some(w)) => {
                if let Some(p) = had {
                    if p.icon.backend == RailBackend::KittyGraphics {
                        down.push_str(&escape_delete(id));
                    } else if p.shown {
                        repaint.push(p.icon.rect);
                    }
                }
                after[i] = None;
                // Marked for a payload, built below outside the lock.
                up.push((w, String::new()));
            }
        }
    }
    if after == before && up.is_empty() {
        return;
    }
    for (w, escape) in &mut up {
        if escape.is_empty() {
            match payload(id_for(w.slot), w) {
                Some(p) => {
                    *escape = p;
                    after[w.slot] = Some(Placed {
                        icon: (*w).clone(),
                        shown: true,
                    });
                }
                None => after[w.slot] = None,
            }
        }
    }
    let written = (|| -> std::io::Result<()> {
        out.write_all(b"\x1b7")?;
        out.write_all(down.as_bytes())?;
        if let Some(buf) = frame {
            out.draw(repaint_cells(buf, &repaint).into_iter())?;
        }
        for (w, escape) in up.iter().filter(|(_, e)| !e.is_empty()) {
            crossterm::queue!(out, crossterm::cursor::MoveTo(w.rect.x, w.rect.y))?;
            out.write_all(escape.as_bytes())?;
        }
        out.write_all(b"\x1b8")?;
        std::io::Write::flush(out)
    })();
    match written {
        Ok(()) => *placed() = after,
        Err(e) => {
            // An APC or DCS cut short swallows whatever is painted after it.
            // What was there is kept: the exit can still erase it.
            let _ = out.write_all(b"\x1b\\");
            tracing::debug!(error = %e, "could not place the panel icons");
        }
    }
}

/// How wide a strip of the frame the loop keeps: the big column is four
/// cells, and its icons sit in columns 1–2.
pub const STRIP_W: u16 = 4;

/// The column's strip of a drawn frame — all a repaint reads — instead of
/// a copy of the whole screen on every frame.
#[must_use]
pub fn column_strip(frame: &ratatui::buffer::Buffer) -> ratatui::buffer::Buffer {
    let area = ratatui::layout::Rect {
        width: frame.area.width.min(STRIP_W),
        ..frame.area
    };
    let mut strip = ratatui::buffer::Buffer::empty(area);
    for p in area.positions() {
        strip[p] = frame[p].clone();
    }
    strip
}

/// The cells of `rects` as `frame` has them, for a repaint — skipping the
/// right half of a wide character and cells ratatui itself skips: writing
/// one would cut the character in two.
fn repaint_cells<'a>(
    frame: &'a ratatui::buffer::Buffer,
    rects: &[ratatui::layout::Rect],
) -> Vec<(u16, u16, &'a ratatui::buffer::Cell)> {
    use unicode_width::UnicodeWidthStr as _;
    rects
        .iter()
        .flat_map(|r| r.positions())
        .filter(|p| frame.area.contains(*p))
        .filter(|p| frame[*p].diff_option != ratatui::buffer::CellDiffOption::Skip)
        .filter(|p| p.x == frame.area.x || frame[(p.x - 1, p.y)].symbol().width() < 2)
        .map(|p| (p.x, p.y, &frame[p]))
        .collect()
}

/// Whether sixel images are up: the loop keeps the drawn frame while they
/// are, to repaint their cells when they come down — even if the backend
/// just stopped being sixel (a config reload).
#[must_use]
pub fn any_sixel_placed() -> bool {
    placed()
        .iter()
        .flatten()
        .any(|p| p.shown && p.icon.backend == RailBackend::Sixel)
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
    // Sixel images are cells, gone with the alternate screen; only kitty's
    // outlive it — shown or hidden, their bytes are in the terminal. One
    // delete per id: the range delete (`d=R`) is newer than some terminals.
    let esc: String = placed
        .iter()
        .enumerate()
        .filter(|(_, p)| {
            p.as_ref()
                .is_some_and(|p| p.icon.backend == RailBackend::KittyGraphics)
        })
        .map(|(i, _)| crate::kitty_graphics::escape_delete(id_for(i)))
        .collect();
    placed.clear();
    if esc.is_empty() {
        return;
    }
    if let Err(e) = out.write_all(esc.as_bytes()).and_then(|()| out.flush()) {
        tracing::debug!(error = %e, "could not erase the panel icons");
    }
}

#[cfg(test)]
// These tests share `PLACED`, PROCESS state: safe under nextest, which runs
// each test in its own process (`just t`); `cargo test` threads would race.
// Each test starts with `delete_all` to begin from nothing.
mod tests {
    use super::*;
    use norte_config::Images;

    fn icon(kind: &str, rgb: [u8; 3], x: u16, y: u16) -> RailIcon {
        RailIcon {
            kind: kind.to_owned(),
            rgb,
            rect: ratatui::layout::Rect::new(x, y, 2, 2),
            canvas: (64, 128),
            backend: RailBackend::KittyGraphics,
            bg: [0, 0, 0],
            slot: 0,
        }
    }

    fn sixel_icon(rgb: [u8; 3]) -> RailIcon {
        RailIcon {
            canvas: canvas_exact((9, 19)),
            backend: RailBackend::Sixel,
            ..icon("places", rgb, 1, 2)
        }
    }

    #[test]
    fn choose_prefers_kitty_and_needs_cells_for_sixel() {
        let px = || (9, 19);
        assert_eq!(
            choose(Images::Auto, true, true, px),
            RailBackend::KittyGraphics
        );
        assert_eq!(choose(Images::Auto, false, true, px), RailBackend::Sixel);
        assert_eq!(
            choose(Images::Auto, false, true, || (0, 0)),
            RailBackend::Glyph
        );
        assert_eq!(choose(Images::Off, false, true, px), RailBackend::Glyph);
        assert_eq!(choose(Images::Blocks, false, true, px), RailBackend::Glyph);
        assert_eq!(
            choose(Images::Kitty, false, true, px),
            RailBackend::KittyGraphics
        );
    }

    /// Sixel does not stretch: the canvas is the slot's pixels, exactly.
    #[test]
    fn the_sixel_canvas_is_the_slot_exactly() {
        assert_eq!(canvas_exact((9, 19)), (18, 38));
    }

    #[test]
    fn sixel_sync_paints_a_dcs_and_never_an_apc() {
        let _ = written(delete_all);
        let out = written(|o| sync_v(o, &[sixel_icon([200, 0, 0])]));
        assert!(out.contains("\x1bP9;1;q\"1;1;18;38"), "{out:?}");
        assert!(!out.contains("\x1b_G"), "{out:?}");
        // A recolour paints over: no delete, a new image.
        let again = written(|o| sync_v(o, &[sixel_icon([0, 200, 0])]));
        assert!(
            again.contains("\x1bP") && !again.contains("\x1b_G"),
            "{again:?}"
        );
    }

    /// `sync` over a `Vec`, through a real ratatui backend.
    fn sync_v(o: &mut Vec<u8>, want: &[RailIcon]) {
        sync_f(o, want, None);
    }

    fn sync_f(o: &mut Vec<u8>, want: &[RailIcon], frame: Option<&ratatui::buffer::Buffer>) {
        sync(&mut ratatui::backend::CrosstermBackend::new(o), want, frame);
    }

    /// A frame whose every cell reads `x`: what a repaint writes is visible.
    fn frame_of_x() -> ratatui::buffer::Buffer {
        let area = ratatui::layout::Rect::new(0, 0, 10, 10);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        for p in area.positions() {
            buf[p].set_symbol("x");
        }
        buf
    }

    /// Hiding a sixel image has no escape to send: its cells are repainted
    /// from the frame, since ratatui does not rewrite cells it believes
    /// unchanged (seen in xterm: icons below an open menu stayed half
    /// drawn). Shown again, it is re-sent.
    #[test]
    fn sixel_hidden_then_shown_emits_again() {
        let _ = written(delete_all);
        let want = [sixel_icon([200, 0, 0])];
        let _ = written(|o| sync_v(o, &want));
        let hidden = written(|o| sync_f(o, &[], Some(&frame_of_x())));
        assert_eq!(hidden.matches('x').count(), 4, "its 2×2 cells: {hidden:?}");
        assert!(!hidden.contains("\x1b_G"), "no kitty delete");
        assert!(written(|o| sync_v(o, &want)).contains("\x1bP"));
    }

    /// An icon that moves one row (a button more, the menu bar toggled)
    /// shares a row with where it was: the old cells must be repainted
    /// BEFORE the new image, or the repaint erases half of it.
    #[test]
    fn a_moved_sixel_is_repainted_before_it_is_placed_again() {
        let _ = written(delete_all);
        let _ = written(|o| sync_v(o, &[sixel_icon([200, 0, 0])]));
        let moved = RailIcon {
            rect: ratatui::layout::Rect::new(1, 1, 2, 2),
            ..sixel_icon([200, 0, 0])
        };
        let out = written(|o| sync_f(o, &[moved], Some(&frame_of_x())));
        let repaint = out.find('x').expect("old cells repainted");
        let image = out.find("\x1bP").expect("placed again");
        assert!(repaint < image, "{out:?}");
    }

    /// A recolour in the same cells repaints them first: the new image's
    /// empty pixels are transparent, and the old strokes would show
    /// through.
    #[test]
    fn a_recoloured_sixel_clears_its_cells_first() {
        let _ = written(delete_all);
        let _ = written(|o| sync_v(o, &[sixel_icon([200, 0, 0])]));
        let out = written(|o| sync_f(o, &[sixel_icon([0, 200, 0])], Some(&frame_of_x())));
        let repaint = out.find('x').expect("cells repainted");
        let image = out.find("\x1bP").expect("placed again");
        assert!(repaint < image, "{out:?}");
    }

    /// Hidden under a menu, a kitty icon keeps its bytes in the terminal
    /// (`d=i`); shown again, it is only re-placed (`a=p`) — closing a menu
    /// used to re-send every PNG.
    #[test]
    fn a_hidden_kitty_icon_comes_back_without_its_bytes() {
        let _ = written(delete_all);
        let want = [icon("places", [1, 2, 3], 1, 2)];
        let _ = written(|o| sync_v(o, &want));
        let hide = written(|o| sync_v(o, &[]));
        assert!(
            hide.contains(&format!("a=d,d=i,i={}", id_for(0))),
            "{hide:?}"
        );
        let show = written(|o| sync_v(o, &want));
        assert!(show.contains("a=p") && !show.contains("a=T"), "{show:?}");
    }

    /// The image id is the column SLOT's, not the position among the icons
    /// placed: a slot with no SVG ahead of others no longer shifts them.
    #[test]
    fn the_id_follows_the_slot() {
        let _ = written(delete_all);
        let mut at_three = icon("places", [1, 2, 3], 1, 2);
        at_three.slot = 3;
        let out = written(|o| sync_v(o, &[at_three]));
        assert!(out.contains(&format!("i={}", id_for(3))), "{out:?}");
    }

    /// Erasing goes id by id — a range delete (`d=R`) is newer than some
    /// terminals — and covers hidden images too.
    #[test]
    fn delete_all_names_each_kitty_image_hidden_or_not() {
        let _ = written(delete_all);
        let _ = written(|o| sync_v(o, &[icon("places", [1, 2, 3], 1, 2)]));
        let _ = written(|o| sync_v(o, &[]));
        let all = written(delete_all);
        assert!(
            all.contains(&format!("d=I,i={}", id_for(0))) && !all.contains("d=R"),
            "{all:?}"
        );
    }

    /// A write that fails keeps what was believed placed, so the exit can
    /// still delete it — dropping it left an orphan image on screen.
    #[test]
    fn a_failed_write_keeps_what_is_on_screen() {
        struct Broken;
        impl std::io::Write for Broken {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("gone"))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let _ = written(delete_all);
        let _ = written(|o| sync_v(o, &[icon("places", [1, 2, 3], 1, 2)]));
        sync(
            &mut ratatui::backend::CrosstermBackend::new(Broken),
            &[],
            None,
        );
        assert!(written(delete_all).contains(&format!("d=I,i={}", id_for(0))));
    }

    /// What the loop keeps of a drawn frame for repaints: only the
    /// column's strip, not the whole screen every frame.
    #[test]
    fn the_kept_frame_is_the_column_strip() {
        let strip = column_strip(&frame_of_x());
        assert_eq!(strip.area, ratatui::layout::Rect::new(0, 0, STRIP_W, 10));
        assert_eq!(strip[(1, 3)].symbol(), "x", "its cells, copied");
    }

    /// A raster that fails is remembered as failed: it was retried — and
    /// logged — on every frame.
    #[test]
    fn a_failed_raster_is_not_retried() {
        let before = RASTERISED.load(std::sync::atomic::Ordering::Relaxed);
        assert!(png("places", [1, 2, 3], (0, 0)).is_none());
        assert!(png("places", [1, 2, 3], (0, 0)).is_none());
        let after = RASTERISED.load(std::sync::atomic::Ordering::Relaxed);
        assert_eq!(after - before, 1, "one attempt");
    }

    /// The repaint does not write the right half of a wide character (an
    /// overlay's CJK title over the column): that would cut it in two.
    #[test]
    fn the_repaint_does_not_split_a_wide_character() {
        let area = ratatui::layout::Rect::new(0, 0, 4, 1);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        buf.set_string(0, 0, "界", ratatui::style::Style::default());
        let cells = repaint_cells(&buf, &[ratatui::layout::Rect::new(0, 0, 3, 1)]);
        let xs: Vec<u16> = cells.iter().map(|(x, _, _)| *x).collect();
        assert_eq!(xs, vec![0, 2], "x=1 is the wide character's other half");
    }

    /// Kitty images come down by escape (hidden, `d=i`): nothing is
    /// repainted.
    #[test]
    fn kitty_removal_repaints_nothing() {
        let _ = written(delete_all);
        let _ = written(|o| sync_v(o, &[icon("places", [1, 2, 3], 1, 2)]));
        let out = written(|o| sync_f(o, &[], Some(&frame_of_x())));
        assert!(!out.contains('x') && out.contains("d=i"), "{out:?}");
    }

    /// The loop keeps the frame while sixel pixels are UP, not only while
    /// the backend is sixel: a reload to `images = "off"` must still be
    /// able to repaint what it takes down.
    #[test]
    fn any_sixel_placed_follows_the_screen() {
        let _ = written(delete_all);
        assert!(!any_sixel_placed());
        let _ = written(|o| sync_v(o, &[sixel_icon([200, 0, 0])]));
        assert!(any_sixel_placed());
        forget();
        assert!(!any_sixel_placed());
    }

    /// A still frame builds no image: encoding every icon on every turn —
    /// quiet ticks included — was an idle-CPU regression.
    #[test]
    fn a_still_frame_builds_nothing() {
        let _ = written(delete_all);
        let want = [icon("places", [1, 2, 3], 1, 2)];
        let _ = written(|o| sync_v(o, &want));
        let before = BUILT.load(std::sync::atomic::Ordering::Relaxed);
        let _ = written(|o| sync_v(o, &want));
        assert_eq!(BUILT.load(std::sync::atomic::Ordering::Relaxed), before);
    }

    /// The cursor goes back where ratatui left it: a visible cursor (the
    /// terminal panel's) must not stay in the column.
    #[test]
    fn the_cursor_is_saved_and_restored() {
        let _ = written(delete_all);
        let out = written(|o| sync_v(o, &[icon("places", [1, 2, 3], 1, 2)]));
        assert!(
            out.starts_with("\x1b7") && out.ends_with("\x1b8"),
            "{out:?}"
        );
    }

    /// The ioctl that measures cells runs only when sixel could use it.
    #[test]
    fn choose_measures_cells_only_for_sixel() {
        let never = || -> (u16, u16) { panic!("measured for nothing") };
        assert_eq!(
            choose(Images::Auto, true, true, never),
            RailBackend::KittyGraphics
        );
        assert_eq!(choose(Images::Off, false, true, never), RailBackend::Glyph);
        assert_eq!(
            choose(Images::Auto, false, false, never),
            RailBackend::Glyph
        );
    }

    /// Sixel pixels are cells: leaving the alternate screen takes them, and
    /// a terminal that never spoke kitty gets no APC on the way out.
    #[test]
    fn delete_all_after_sixel_writes_nothing() {
        let _ = written(delete_all);
        let _ = written(|o| sync_v(o, &[sixel_icon([200, 0, 0])]));
        assert_eq!(written(delete_all), "");
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
        let _ = written(|o| sync_v(o, &want));
        forget();
        assert!(written(|o| sync_v(o, &want)).contains("a=T"));
    }

    /// The panic hook runs on the panicking thread, which may hold the lock
    /// (a panic inside `sync`): it must return, writing nothing, instead of
    /// deadlocking before the terminal leaves the alternate screen.
    #[test]
    fn delete_all_under_a_held_lock_returns() {
        let _ = written(delete_all);
        let _ = written(|o| sync_v(o, &[icon("places", [1, 2, 3], 1, 2)]));
        let guard = PLACED.lock().expect("not poisoned");
        assert_eq!(written(delete_all), "");
        drop(guard);
    }

    /// Runs `f` against a fresh writer, after forgetting whatever an
    /// earlier step placed, and hands back what it wrote.
    fn written<R>(f: impl FnOnce(&mut Vec<u8>) -> R) -> String {
        let mut out = Vec::new();
        let _ = f(&mut out);
        String::from_utf8(out).expect("escapes are ascii")
    }

    #[test]
    fn the_first_sync_places_and_the_second_writes_nothing() {
        let _ = written(delete_all);
        let want = [icon("places", [1, 2, 3], 1, 2)];
        let first = written(|o| sync_v(o, &want));
        assert!(first.contains(&format!("a=T,i={}", id_for(0))), "{first:?}");
        assert!(first.contains("c=2,r=2"), "{first:?}");
        assert_eq!(written(|o| sync_v(o, &want)), "", "nothing changed");
    }

    #[test]
    fn a_new_colour_replaces_the_old_image() {
        let _ = written(delete_all);
        let _ = written(|o| sync_v(o, &[icon("places", [1, 2, 3], 1, 2)]));
        let again = written(|o| sync_v(o, &[icon("places", [9, 9, 9], 1, 2)]));
        let del = again
            .find(&format!("d=I,i={}", id_for(0)))
            .expect("deleted");
        let put = again.find(&format!("a=T,i={}", id_for(0))).expect("placed");
        assert!(del < put, "delete first: {again:?}");
    }

    #[test]
    fn shrinking_drops_to_glyphs_and_erases() {
        let _ = written(delete_all);
        let _ = written(|o| sync_v(o, &[icon("places", [1, 2, 3], 1, 2)]));
        let gone = written(|o| sync_v(o, &[]));
        assert!(gone.contains(&format!("d=i,i={}", id_for(0))), "{gone:?}");
        assert_eq!(written(|o| sync_v(o, &[])), "", "hidden stays hidden");
    }

    /// The exit paths call it on EVERY terminal: one that never got an icon
    /// must not receive an APC either.
    #[test]
    fn delete_all_with_nothing_placed_writes_nothing() {
        let _ = written(delete_all);
        assert_eq!(written(delete_all), "");
    }

    #[test]
    fn delete_all_clears_the_state() {
        let want = [icon("places", [1, 2, 3], 1, 2)];
        let _ = written(|o| sync_v(o, &want));
        let _ = written(delete_all);
        assert!(
            written(|o| sync_v(o, &want)).contains("a=T"),
            "placed again"
        );
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
}
