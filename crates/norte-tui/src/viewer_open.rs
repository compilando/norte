//! Opening the viewer over a file, and moving the one already open.
//!
//! It used to live in the `ntc` binary's root — a crate DISTINCT from this
//! lib — so neither the integration tests nor the background preview fetch
//! could reach it without the event loop acting as a go-between.

use norte_core::backend::Backend;
use norte_i18n::{t, ta};
use norte_proto::{Error, VPath};

use crate::app::{App, error_category};
use crate::console::Waited;
use crate::viewer::Viewer;
use norte_frontend::busy::{Busy, BusyKind};

/// How this image is going to be shown, with the key and the terminal
/// already resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Modo {
    /// Pixels via the terminal's protocol.
    Kitty,
    /// Half blocks, placed by an approved previewer.
    Bloques,
    /// Nothing: the viewer keeps the bytes.
    Nada,
}

/// Resolves `[ui] images` against what the probe answered.
///
/// `Bloques` is NOT a branch that does anything: it is "do nothing
/// special", and the image previewer — if approved and enabled — already
/// paints. That is why `Bloques` and `Nada` look so alike here and are told
/// apart in the help: with `off` the reader asked for hexview; with
/// `blocks` they asked for half blocks and what is missing is approving the
/// plugin.
#[must_use]
pub fn modo_efectivo(cfg: norte_config::Images, soporta: bool) -> Modo {
    match cfg {
        norte_config::Images::Off => Modo::Nada,
        norte_config::Images::Kitty => Modo::Kitty,
        norte_config::Images::Auto if soporta => Modo::Kitty,
        // `Blocks` and `Auto`'s fallback with no support are the SAME
        // branch (clippy `match_same_arms`): both want "do not paint
        // pixels, let the previewer do it" — the distinction lives in the
        // help, not in the code.
        norte_config::Images::Blocks | norte_config::Images::Auto => Modo::Bloques,
    }
}

/// The viewer bar's notice when the slot has nobody to paint the image.
///
/// The real pilot found this hole: with no approved previewer, a PNG in
/// `Modo::Bloques` falls back to hexview exactly like a file nobody knows
/// how to interpret, and nothing on screen tells the two cases apart. A
/// silent hexview is indistinguishable from "norte does not know how".
///
/// `no_hace_falta_avisar` is `true` when this slot does NOT need the
/// notice. The caller decides WHAT that means depending on `modo` —
/// passing [`no_hace_falta_avisar_de_imagen`] in [`Modo::Bloques`] or
/// [`no_hace_falta_avisar_de_miniatura`] in [`Modo::Kitty`] — instead of
/// repeating the expression inline at the call site (fix round 1, phase 5:
/// a `hay_previewer` computed there, under that name, invited
/// "simplifying" it to `viewer.preview_plugin().is_some()`, which loses
/// the first reason and would warn for any non-image file).
///
/// Task 5b (a T6 review finding): this function's original docstring said
/// that in [`Modo::Kitty`] "the terminal already paints pixels on its
/// own… there is nothing to approve." That is FALSE — the bytes Kitty
/// places come from a `thumbnail` plugin (`plugins/image-thumb`), just as
/// optional and approvable as [`Modo::Bloques`]'s `previewer`; with none
/// approved the reader is left on hexview just as silently as in the other
/// branch, which is exactly the hole this function exists to close. Both
/// modes now warn, with DIFFERENT texts: they ask to approve different
/// EXTENSIONS, and sending the reader to approve the wrong one is worse
/// than not warning. In [`Modo::Nada`] the reader asked for hexview
/// themselves (`images = "off"`): there is nothing to approve there and no
/// warning is given.
#[must_use]
pub fn aviso_de_imagen(
    modo: Modo,
    no_hace_falta_avisar: bool,
    formato_ajeno: bool,
) -> Option<String> {
    if no_hace_falta_avisar {
        return None;
    }
    match modo {
        Modo::Bloques => Some(t("viewer-image-needs-previewer")),
        // The two reasons there are no pixels in Kitty ask for DIFFERENT
        // things from the reader, and only one is fixed from F12. Sending
        // them to approve what is already approved is worse than saying
        // nothing: the reader goes, finds everything in order, and is left
        // with no clue.
        Modo::Kitty if formato_ajeno => Some(t("viewer-image-thumbnail-format")),
        Modo::Kitty => Some(t("viewer-image-needs-thumbnail")),
        Modo::Nada => None,
    }
}

/// Whether `viewer` does NOT need [`aviso_de_imagen`]'s notice in
/// [`Modo::Bloques`] — the second parameter that call site passes it when
/// the mode is that one.
///
/// It is `!viewer.is_image()`, and [`Viewer::is_image`] already ANDs the
/// two conditions needed: `plugin_preview.is_none() && image.is_some()` —
/// only `true` when NO previewer replaced the view AND the bytes are a
/// recognized image. Negating it gives "it is not an image, or it IS but a
/// previewer already painted": the two reasons not to warn, together.
///
/// T3 (this same phase) already warned that the TUI must not use
/// `is_image()` to decide "is an image" (it stops painting pixels the
/// moment a previewer replaces the view); here it is the other way
/// around — it is used on purpose, TO know whether something already
/// replaced the view — but the sibling trap exists: do not "fix" it to
/// `viewer.preview_plugin().is_some()` thinking it more honest. That loses
/// the non-image half and would warn about a missing IMAGE previewer for
/// any file that is not an image in `Modo::Bloques` — exactly the
/// regression centralizing this computation here, under this name, exists
/// to prevent.
///
/// See [`no_hace_falta_avisar_de_miniatura`] for [`Modo::Kitty`]'s
/// counterpart, which asks for a `thumbnail` plugin, not a `previewer`.
#[must_use]
pub fn no_hace_falta_avisar_de_imagen(viewer: &Viewer) -> bool {
    !viewer.is_image()
}

/// Whether `viewer` does NOT need [`aviso_de_imagen`]'s notice in
/// [`Modo::Kitty`] — [`no_hace_falta_avisar_de_imagen`]'s counterpart for
/// the `thumbnail` plugin instead of the `previewer`.
///
/// `true` when EITHER of two different things already makes the notice
/// unneeded: `!viewer.is_image()` — the file is not an image, or it IS but
/// a plugin previewer already replaced the raw view and half blocks are
/// already being painted ("if the image is already being seen… there is
/// nothing to warn about", same as in [`Modo::Bloques`]) — OR `imagen`
/// carries a thumbnail already PLACED for THIS file. Comparing `imagen`'s
/// `path` against `viewer`'s matters: the reader may still be looking at
/// one file's hexview while ANOTHER's (the one they were looking at
/// before) thumbnail is still alive in [`App::viewer_imagen`] waiting for
/// the run loop to erase it — that old thumbnail says nothing about
/// whether THIS file has its own.
#[must_use]
pub fn no_hace_falta_avisar_de_miniatura(viewer: &Viewer, imagen: Option<&ImagenColocada>) -> bool {
    !viewer.is_image() || imagen.is_some_and(|imagen| imagen.path == viewer.path)
}

/// A thumbnail already requested and ready to place (T4 places/erases it).
///
/// Lives in [`App`], not in [`Viewer`]: `Viewer` belongs to `norte-frontend`
/// and both frontends share it, and the window already has its own path to
/// thumbnails — putting a TUI field there would dirty a shared surface.
#[derive(Debug, Clone)]
pub struct ImagenColocada {
    /// The file this thumbnail belongs to — to know whether it is still
    /// the one the viewer shows once the reader has already moved to
    /// another one.
    pub path: VPath,
    /// The encoded bytes the plugin returned — ALWAYS `"image/png"` (see
    /// [`imagen_desde_miniatura`]): it is the only format kitty knows how
    /// to place with `f=100`, so nothing that gets here is anything else.
    pub bytes: Vec<u8>,
    /// The mimetype the plugin said — stored so the invariant above
    /// (always PNG) is CHECKABLE, not just documented.
    pub mimetype: String,
    /// Width in pixels, as stated by the raster's header.
    pub width: u32,
    /// Height in pixels, as stated by the raster's header.
    pub height: u32,
    /// The id it is placed and erased with by kitty's protocol.
    pub id: u32,
    /// Where it was placed last time (T4 paints it and fills this in);
    /// `None` until the first frame that places it.
    ///
    /// It is the WHOLE PLACEMENT and not just the rect (spec 2026-09-20):
    /// with zoom, two frames can occupy the same cells and show different
    /// pieces of the image, and comparing only the rect would leave the
    /// screen looking still while the reader moves around inside it.
    pub puesta_en: Option<Colocacion>,
}

/// Where the image goes and which part of it is seen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Colocacion {
    /// The cells it occupies.
    pub rect: ratatui::layout::Rect,
    /// The piece of the raster that is shown, in pixels. `None` = whole.
    pub recorte: Option<Recorte>,
}

/// A piece of the raster, in the image's own pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Recorte {
    /// Offset from the left.
    pub x: u32,
    /// Offset from the top.
    pub y: u32,
    /// The piece's width.
    pub w: u32,
    /// The piece's height.
    pub h: u32,
}

/// Where to place an image with the zoom the viewer has.
///
/// Three regimes, and the reason there are three is that a terminal cannot
/// paint outside the viewer's slot:
///
/// - **Fit** (100%): the image is stretched to the whole slot, which is
///   what it always did.
/// - **Zoomed out** (< 100%): the slot given to it SHRINKS, and the whole
///   image still fits inside. Nothing is cropped.
/// - **Zoomed in** (> 100%): the slot is the same and what shrinks is the
///   PIECE of the raster that is shown. That is magnifying, and it is what
///   lets the keys that move the viewer be used to pan around inside it.
///
/// `pan_x`/`pan_y` are the requested offset, in cells; they are translated
/// into raster pixels and clamped so the piece does not go out of bounds.
#[must_use]
pub fn colocacion(
    zoom_pct: u16,
    slot: ratatui::layout::Rect,
    width: u32,
    height: u32,
    pan_x: usize,
    pan_y: usize,
) -> Colocacion {
    use ratatui::layout::Rect;
    if slot.is_empty() || width == 0 || height == 0 {
        return Colocacion {
            rect: slot,
            recorte: None,
        };
    }
    if zoom_pct < 100 {
        // Shrinks the slot. Never to zero: `c=0,r=0` means to kitty "the
        // image's natural size", which over the whole screen is exactly
        // what [`imagen_a_colocar`]'s guard prevents.
        let scale = |v: u16| {
            u16::try_from(u32::from(v) * u32::from(zoom_pct) / 100)
                .unwrap_or(u16::MAX)
                .max(1)
        };
        return Colocacion {
            rect: Rect {
                width: scale(slot.width),
                height: scale(slot.height),
                ..slot
            },
            recorte: None,
        };
    }
    if zoom_pct == 100 {
        return Colocacion {
            rect: slot,
            recorte: None,
        };
    }
    // Zooming in: the visible piece is the inverse of the zoom, and at
    // least one pixel — a piece of zero is not a small image, it is none.
    let pct = u32::from(zoom_pct);
    let w = (width * 100 / pct).max(1).min(width);
    let h = (height * 100 / pct).max(1).min(height);
    // The pan is requested in CELLS and spent here in pixels: one cell of
    // movement moves the same fraction of the image a slot cell occupies,
    // which is what makes moving feel the same at any zoom.
    let step_x = w / u32::from(slot.width).max(1);
    let step_y = h / u32::from(slot.height).max(1);
    let x = u32::try_from(pan_x)
        .unwrap_or(u32::MAX)
        .saturating_mul(step_x)
        .min(width - w);
    let y = u32::try_from(pan_y)
        .unwrap_or(u32::MAX)
        .saturating_mul(step_y)
        .min(height - h);
    Colocacion {
        rect: slot,
        recorte: Some(Recorte { x, y, w, h }),
    }
}

/// What came out of requesting a file's thumbnail, with the REASON when
/// there is none to place.
///
/// An `Option<ImagenColocada>` used to say "there is none" and nothing
/// more, and the two "there is none"s ask for different things from the
/// reader: with no approved `thumbnail` plugin one has to go to F12 and
/// approve it; with one approved that answered in JPEG there is nothing to
/// approve, and that same notice sends them to a screen where everything
/// looks fine. A viewer that asks for the impossible is worse than a
/// silent one.
#[derive(Debug, Clone, Default)]
pub enum Miniatura {
    /// There was none: no approved and enabled `thumbnail` plugin, the
    /// call failed, or the mode did not ask for a thumbnail.
    #[default]
    Ninguna,
    /// A plugin answered, but in a format kitty does not know how to
    /// place — PNG only — so it was dropped ([`imagen_desde_miniatura`]).
    FormatoAjeno,
    /// Ready to place.
    Colocable(ImagenColocada),
}

impl Miniatura {
    /// The image, if there is one; drops the reason.
    #[must_use]
    pub fn colocable(self) -> Option<ImagenColocada> {
        match self {
            Self::Colocable(imagen) => Some(imagen),
            Self::Ninguna | Self::FormatoAjeno => None,
        }
    }
}

/// The next image id never used before in this process.
///
/// Belongs to this module — not to [`App`]'s `SlotId` counter, which is
/// private to its own module and unreachable from here — and is never
/// reused for the same reason as that one: a recycled id could erase or
/// replace another placement's image in flight.
fn mint_image_id() -> u32 {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// Converts what `plugin.thumbnail` returned into an [`ImagenColocada`], or
/// says WHY there is none to place ([`Miniatura`]).
///
/// Branch review, finding 1: `escape_colocar` sends a FIXED `f=100` — kitty's
/// protocol has no `f=` key for JPEG nor for WebP, only PNG (100) or raw
/// raster (24/32) — but
/// [`norte_proto::methods::PluginThumbnail::mimetype`] allows all three
/// (`thumb::reencode` in `norte-plugin-host` writes PNG and falls back to
/// JPEG quality 85 when the PNG does not fit in 4 MiB, easy with this
/// viewer's `max_edge` of up to 1920 px). Without this filter, a JPEG
/// travels with a header that says PNG: kitty rejects it, `q=2` silences
/// the error, [`crate::kitty_graphics::marcar_colocada`] already noted the
/// id so nobody retries, and [`no_hace_falta_avisar_de_miniatura`] sees an
/// [`ImagenColocada`] for this file and silences the notice — an empty
/// viewer with no trace of why.
///
/// Dropping it returns the notice, but the "an extension needs approving"
/// notice is FALSE in this specific case: the extension is approved and
/// enabled, it answered, and what does not work is its format. Sending the
/// reader to F12 to approve what is already approved is a dead end. That
/// is why this returns [`Miniatura::FormatoAjeno`] and not a `None` with
/// no reason: the notice that comes out then is a different one and says
/// what is happening.
#[must_use]
pub fn imagen_desde_miniatura(
    path: &VPath,
    thumb: norte_proto::methods::PluginThumbnail,
) -> Miniatura {
    if thumb.mimetype != "image/png" {
        return Miniatura::FormatoAjeno;
    }
    Miniatura::Colocable(ImagenColocada {
        path: path.clone(),
        bytes: thumb.bytes,
        mimetype: thumb.mimetype,
        width: thumb.width,
        height: thumb.height,
        id: mint_image_id(),
        puesta_en: None,
    })
}

impl App {
    /// Closes the full-screen viewer AND its thumbnail AT THE SAME TIME.
    ///
    /// The invariant is that there cannot be an [`App::viewer_imagen`] with
    /// no [`App::viewer`] to match it — otherwise T4 places or erases by an
    /// id that no longer has a viewer behind it. A loose `app.viewer =
    /// None` at the spot that closes the viewer is exactly the review
    /// finding this fixes: the old thumbnail was left hanging around. A
    /// single closing point makes the invariant impossible to break by
    /// accident at some new spot, instead of having to remember both
    /// fields every time.
    pub fn close_viewer(&mut self) {
        self.viewer = None;
        self.viewer_imagen = None;
        // And the reason there was no image: with no viewer there is
        // nobody to warn, and leaving it set would make the NEXT viewer for
        // the same file inherit a notice nobody has re-checked.
        self.viewer_miniatura_ajena = None;
        // `viewer_modo` with no viewer means nothing — it is left at
        // `Nada` like `App::new`, so a stale `viewer_modo` (Finding 3) does
        // not survive this viewer and confuse whichever one opens next
        // before `open_viewer` sets it again.
        self.viewer_modo = Modo::Nada;
    }

    /// Releases the placed thumbnail when the EFFECTIVE mode (just
    /// resolved against the reloaded config) stopped being [`Modo::Kitty`]
    /// — called ONLY from [`crate::config_reload::reload_config`], after
    /// reassigning `App::chrome`.
    ///
    /// Branch review, finding 3: without this, a `Kitty` that switches to
    /// `off` or `blocks` on a hot reload leaves the pixels already placed
    /// on screen FOREVER — nothing looks at them again once
    /// [`App::viewer_modo`] was pinned when it opened, and the help
    /// promises that `off` "just leaves the viewer on hexview", a promise
    /// only kept if something releases the old thumbnail.
    ///
    /// On purpose it does NOTHING in the opposite direction
    /// (`blocks`/`off` → `kitty`, or any change while already in
    /// `Bloques`/`Nada`): updating the pinned mode there would resurrect the
    /// same review's other hole — the "an extension needs approving for
    /// thumbnails" notice would show for a file the new mode NEVER asked
    /// one for. Returns whether it released something, only so the caller
    /// can log it if it wants to; nobody uses it today.
    pub fn soltar_miniatura_si_deja_de_ser_kitty(&mut self, modo_efectivo: Modo) -> bool {
        if self.viewer.is_none() || self.viewer_modo != Modo::Kitty || modo_efectivo == Modo::Kitty
        {
            return false;
        }
        self.viewer_imagen = None;
        self.viewer_modo = modo_efectivo;
        true
    }
}

/// Applies `f` to whichever viewer has the keyboard.
///
/// The docked one (focused preview) or the full-screen one, in that order:
/// it is what lets the `viewer.*` keys skip a second vocabulary for the
/// preview (L3).
pub fn viewer_do(app: &mut App, f: impl FnOnce(&mut Viewer)) {
    // To whichever viewer has the keyboard. With the docked preview
    // focused, the `viewer.*` keys move THAT one, with no new bindings and
    // no second vocabulary: it is the same viewer in another spot (L3).
    if app.key_owner() == crate::app::KeyOwner::Preview {
        if let Some(id) = app.preview_slot()
            && let Some(v) = app.panes.preview_mut(id).and_then(|p| p.viewer_mut())
        {
            f(v);
        }
        return;
    }
    if let Some(v) = &mut app.viewer {
        f(v);
    }
}

/// Opens the next (or previous) sibling of the same class, without leaving.
///
/// Serves BOTH viewers, like [`viewer_do`], and the same way in both spots
/// except for the finish: with the DOCKED one, moving the cursor is
/// enough — the preview follows the pointed-at row and re-reads on its own
/// on the next turn; full-screen it has to open, because there the viewer
/// follows nobody.
///
/// The starting row is looked up by PATH and not by the cursor: under a
/// quick search filter "what is pointed at" is not the cursor's row, and
/// the viewer may have been opened from exactly there.
pub async fn viewer_sibling(
    app: &mut App,
    backend: &Backend,
    events: &mut crate::console::Console<'_>,
    forward: bool,
) {
    let acoplado = app.key_owner() == crate::app::KeyOwner::Preview;
    // What is open and what class it is. The class is told by the BYTES
    // the viewer already read, not the extension: a photo saved as `.dat`
    // still leads to the next photo.
    let actual = if acoplado {
        app.preview_slot()
            .and_then(|id| app.panes.preview(id))
            .and_then(|p| {
                Some((
                    p.shown()?.clone(),
                    p.viewer().is_some_and(Viewer::is_image_by_bytes),
                ))
            })
    } else {
        app.viewer
            .as_ref()
            .map(|v| (v.path.clone(), v.is_image_by_bytes()))
    };
    let Some((abierta, es_imagen)) = actual else {
        return;
    };
    let quiero = if es_imagen {
        norte_frontend::viewer::Clase::Imagen
    } else {
        norte_frontend::viewer::Clase::Otro
    };
    // Which listing the ladder comes from. With the DOCKED viewer, the one
    // that slot FOLLOWS — which is not always the focused one; with the
    // large one, the focused one. It is the same resolution `preview::want`
    // uses to decide what the preview shows, and it has to be: looking at
    // a different listing would move a panel's cursor and leave the
    // preview exactly as it was.
    let seguido = if acoplado {
        let mut diags = Vec::new();
        app.preview_slot().and_then(|slot| {
            norte_frontend::layout::resolve_follow(&app.layout, slot, &app.roles, &mut diags)
                .or_else(|| app.roles.get(norte_frontend::layout::RoleId::Active))
        })
    } else {
        None
    };
    let pane = match seguido {
        Some(id) => app.panes.browser(id),
        None => Some(app.focused()),
    };
    let Some(pane) = pane else {
        return;
    };
    let entries = pane.entries();
    // Only by what the reader SEES: with a live filter the ladder is its
    // own.
    let visibles = pane.quick_visible();
    let destino = entries
        .iter()
        .position(|e| e.path == abierta)
        .and_then(|from| norte_frontend::viewer::hermana(entries, visibles, from, forward, quiero))
        .and_then(|i| entries.get(i).map(|e| (i, e.path.clone())));
    let Some((fila, path)) = destino else {
        app.message = Some(t("msg-viewer-no-sibling"));
        return;
    };
    // A previous "no more" cannot survive a jump that DID happen.
    app.message = None;
    // `senalar` and not `set_cursor`: with a live filter what is pointed
    // at is the quick search's selection, and the docked one follows THAT.
    // It is ALWAYS pointed at, which is what makes the docked one notice
    // and what leaves the listing where the reader was looking when they
    // close it.
    match seguido {
        Some(id) => {
            if let Some(p) = app.panes.browser_mut(id) {
                p.senalar(fila);
            }
        }
        None => app.focused_mut().senalar(fila),
    }
    if !acoplado {
        open_viewer(app, backend, events, path).await;
    }
}

/// The viewer's read budget: a 256 KiB header (the rest of the file is NOT
/// read — ADR 0005's scope; "load more" = M2 debt). WATCH OUT if this
/// grows (>~1 MiB): `Viewer::recompute` and `rows()` run on the loop's
/// thread — `spawn_blocking` + a line index would be needed.
const VIEW_CAP: u64 = 256 * 1024;

/// Reads `path`'s header and builds its [`Viewer`], with the plugin
/// preview chain and all its degradations.
///
/// NOT cancelable: the caller sets up the `select!` if it has someone
/// waiting in front ([`open_viewer`] does, so `Esc` can abandon it). The
/// docked preview cannot do that — nobody is waiting: the reader keeps
/// moving around the listing — and that is why the read and its modal
/// wrapper are two separate things since L3.
///
/// The degradation order is the contract (ADR 0037): STYLED plugin
/// preview, then plain preview, then the raw view. An `Ok(None)` — no
/// previewer applies, a guest crashed, or the wire's caps were violated —
/// and a network failure degrade THE SAME way: a broken plugin never
/// stops the file from being seen.
/// # Errors
///
/// Whatever the `Backend` returns when reading `path`'s header, untranslated:
/// the caller tells a `PermissionDenied` apart from a `NotFound` to say
/// different things. A plugin previewer's failure is NOT an error — it
/// degrades to the raw view, which is the contract above.
pub async fn viewer_for(
    backend: &Backend,
    path: &VPath,
    modo: Modo,
) -> Result<(Viewer, Miniatura), Error> {
    // The terminal's width is the full-screen viewer's, and it is what an
    // image previewer uses to shrink (proto 0.66.0). With no terminal —
    // tests, a pipe — there is no hint and the guest picks its own width.
    let columns = crossterm::terminal::size()
        .ok()
        .map(|(cols, _)| u32::from(cols));
    viewer_for_width(backend, path, columns, modo).await
}

/// [`viewer_for`] with the width the caller states (a slot's docked viewer
/// is narrower than the screen).
///
/// `modo` also decides whether the thumbnail is requested
/// ([`ImagenColocada`]): only when the BYTES say it is an image
/// ([`norte_frontend::viewer::image_format`]) AND the mode is
/// [`Modo::Kitty`]. With [`Modo::Bloques`] or [`Modo::Nada`] nothing is
/// requested here — `Bloques` is painted by the plugin previewer through
/// its normal path (`plugin_preview_styled` below), not this one.
///
/// On purpose `viewer.is_image()` is NOT used: that getter is `false` the
/// moment a plugin previewer (styled or plain) replaces the raw view, so
/// deciding by it would leave the thumbnail never requested once there was
/// an approved image previewer — which is precisely the case this key
/// exists to solve: the terminal's protocol WINS over the previewer, not
/// the other way around (review finding: T3 phase 5).
///
/// # Errors
///
/// The same as [`viewer_for`]: whatever the `Backend` returns reading the
/// header; a broken previewer degrades, it does not fail. A failure
/// requesting the thumbnail is ALSO not an error: `None` and the viewer
/// looks the same, with no pixels (ADR 0037).
pub async fn viewer_for_width(
    backend: &Backend,
    path: &VPath,
    columns: Option<u32>,
    modo: Modo,
) -> Result<(Viewer, Miniatura), Error> {
    let (bytes, truncated) = read_head(backend, path).await?;
    // By BYTES, before the plugin preview chain — which can replace the
    // whole raw view — gets a chance to hide the format. See the rustdoc
    // above.
    let es_imagen = norte_frontend::viewer::image_format(&bytes).is_some();
    let mut viewer = match backend.plugin_preview_styled(path, columns).await {
        Ok(Some(p)) => {
            Viewer::with_plugin_preview_styled(path.clone(), p.plugin_name, &p.lines, p.lossy)
        }
        Ok(None) | Err(_) => match backend.plugin_preview(path).await {
            Ok(res) => match res.preview {
                Some(p) => {
                    Viewer::with_plugin_preview(path.clone(), p.plugin_name, &p.output, p.lossy)
                }
                None => Viewer::new(path.clone(), bytes, truncated),
            },
            // A broken plugin does not block the file: the usual raw view.
            Err(_) => Viewer::new(path.clone(), bytes, truncated),
        },
    };
    // The BYTES verdict survives a previewer replacing the view: what the
    // FILE is does not change because a plugin won, and the reel
    // (`viewer.next`) depends on that.
    viewer.set_image_by_bytes(es_imagen);
    let miniatura = if es_imagen && modo == Modo::Kitty {
        // The larger side in PIXELS that fits in the slot. A terminal cell
        // is roughly 8x16 px and there is no portable way to ask, so it is
        // estimated: overshooting only costs the terminal shrinking it,
        // undershooting looks blurry.
        let max_edge = columns.unwrap_or(80).saturating_mul(8).clamp(64, 1920);
        backend
            .plugin_thumbnail(path, max_edge)
            .await
            .ok()
            .flatten()
            .map_or(Miniatura::Ninguna, |thumb| {
                imagen_desde_miniatura(path, thumb)
            })
    } else {
        Miniatura::Ninguna
    };
    Ok((viewer, miniatura))
}

/// Opens the full-screen viewer reading the HEADER via the core (rule 7),
/// cancelable like the cd (Esc abandons, Ctrl-C quits).
pub async fn open_viewer(
    app: &mut App,
    backend: &Backend,
    events: &mut crate::console::Console<'_>,
    path: VPath,
) {
    // #323: fetching a REMOTE file's header is another wait that eats the
    // loop. The panel does not change while it lasts, so with no F3
    // indicator a file on a bucket looked exactly like a key that did
    // nothing.
    let started = std::time::Instant::now();
    app.busy = Some(Busy::new(
        BusyKind::Opening,
        Some(path.clone()),
        Some(app.focus()),
    ));
    let modo = modo_efectivo(app.chrome.images(), crate::kitty_graphics::soportado());
    let waited =
        crate::console::wait_painting(events, app, started, viewer_for(backend, &path, modo)).await;
    app.busy = None;
    match waited {
        Waited::Done(Ok((viewer, miniatura))) => {
            // The mismatched format is noted BEFORE consuming the
            // thumbnail, and against THIS path: it is what tells apart
            // "there is no thumbnail extension" from "there is one, it
            // answered, and its format does not work."
            app.viewer_miniatura_ajena =
                matches!(miniatura, Miniatura::FormatoAjeno).then(|| path.clone());
            app.viewer = Some(viewer);
            app.viewer_imagen = miniatura.colocable();
            // Finding 3: the mode the thumbnail was REQUESTED with, set
            // here and not recomputed later — see `App::viewer_modo`'s
            // rustdoc.
            app.viewer_modo = modo;
        }
        Waited::Done(Err(e)) => {
            app.message = Some(ta("msg-view-error", &[("error", &error_category(&e))]));
        }
        Waited::Cancelled => {}
        Waited::Quit => app.quit = true,
    }
}

/// Reads up to `VIEW_CAP + 1` bytes: the extra byte gives away the
/// truncation.
async fn read_head(backend: &Backend, path: &VPath) -> Result<(Vec<u8>, bool), Error> {
    let mut out = backend
        .read(
            path,
            Some(norte_proto::ByteRange {
                offset: 0,
                len: Some(VIEW_CAP + 1),
            }),
        )
        .await?;
    let truncated = out.len() as u64 > VIEW_CAP;
    if truncated {
        out.truncate(usize::try_from(VIEW_CAP).unwrap_or(usize::MAX));
    }
    Ok((out, truncated))
}
