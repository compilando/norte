//! Task 3 (phase 5 WOW): the decision of which mode the viewer uses for an
//! image, resolved against `[ui] images` and what the kitty probe answered
//! — see `viewer_open::modo_efectivo` — and two invariants that fix round 1
//! left as a regression: `App.viewer_imagen` cannot stay dangling when the
//! viewer closes, and it is not decided by `Viewer::is_image()` (which a
//! plugin previewer turns off).

use norte_config::Images;
use norte_proto::VPath;
use norte_proto::methods::PluginThumbnail;
use norte_tui::app::{App, Modal, Pane};
use norte_tui::kitty_graphics::{escape_borrar, escape_colocar};
use norte_tui::viewer_open::{
    ImagenColocada, Miniatura, Modo, aviso_de_imagen, imagen_desde_miniatura, modo_efectivo,
};
use ratatui::layout::Rect;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("test wire")
}

/// Bytes of a PNG that `norte_encoding::detect` classifies as BINARY, not
/// just the 8 bytes of the magic signature: with only the signature, with
/// no NUL byte, the detection heuristic takes it for 8-byte text in a
/// single-byte encoding (`Viewer::recompute` then sets `self.image = None`,
/// text branch) and `is_image()` comes out `false` even though the bytes DO
/// start with the PNG signature — a real regression, caught while writing
/// these tests. Same pattern `fila_de_estado_con_png_sin_previewer` already
/// used (signature + `IHDR` + zero padding up to 40 bytes).
fn png_bytes_binarios() -> Vec<u8> {
    let mut bytes = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR".to_vec();
    bytes.resize(40, 0);
    bytes
}

fn app_en(dir: &VPath) -> App {
    App::new(
        Pane::new(dir.clone(), Vec::new()),
        Pane::new(dir.clone(), Vec::new()),
    )
}

#[test]
fn auto_usa_kitty_solo_si_el_terminal_sabe() {
    assert_eq!(modo_efectivo(Images::Auto, true), Modo::Kitty);
    assert_eq!(modo_efectivo(Images::Auto, false), Modo::Bloques);
}

#[test]
fn kitty_forzado_manda_aunque_la_sonda_dijera_que_no() {
    // The probe can be wrong — a multiplexer with passthrough, a terminal
    // that does not answer but knows — and forcing exists for that. If it
    // truly does not know, what you see is garbage on screen, and that is
    // why it is not the default value.
    assert_eq!(modo_efectivo(Images::Kitty, false), Modo::Kitty);
}

#[test]
fn blocks_no_usa_kitty_aunque_el_terminal_sepa() {
    assert_eq!(modo_efectivo(Images::Blocks, true), Modo::Bloques);
}

#[test]
fn off_no_pinta_nada_y_deja_el_visor_como_estaba() {
    assert_eq!(modo_efectivo(Images::Off, true), Modo::Nada);
}

/// FINDING 1 of fix round 1: `Command::ViewerClose` set `app.viewer = None`
/// without touching `app.viewer_imagen`, so after viewing an image with a
/// thumbnail and closing the viewer, the previous image's thumbnail stayed
/// alive — dangling until T4 uses it to place/delete by id. `App::close_viewer`
/// clears both at once; this test pins that invariant directly on the
/// method, without going through `dispatch` (which needs a `Backend` this
/// test does not).
#[test]
fn cerrar_el_visor_limpia_tambien_su_miniatura() {
    let dir = vp("mem:///");
    let mut app = app_en(&dir);
    app.viewer = Some(norte_tui::viewer::Viewer::new(
        vp("mem:///x.png"),
        b"\x89PNG\r\n\x1a\n".to_vec(),
        false,
    ));
    app.viewer_imagen = Some(ImagenColocada {
        path: vp("mem:///x.png"),
        bytes: vec![0u8; 4],
        mimetype: "image/png".to_owned(),
        width: 8,
        height: 4,
        id: 7,
        puesta_en: None,
    });

    app.close_viewer();

    assert!(app.viewer.is_none(), "the viewer closes");
    assert!(
        app.viewer_imagen.is_none(),
        "and its thumbnail goes WITH it, it does not stay dangling"
    );
}

/// FINDING 2 of fix round 1: deciding whether to request the thumbnail by
/// `Viewer::is_image()` left the whole phase dead as soon as an image
/// plugin previewer (`image-ansi`, in this very repo) was approved, because
/// that getter is `false` as soon as the plugin preview replaces the raw
/// view. `viewer_open::viewer_for_width` decides from the BYTES
/// (`image_format`) before the plugin chain has a chance to hide the
/// format. This test records the cross-case that justifies it: the two can
/// disagree about the SAME file.
///
/// It does not cover the whole path (`viewer_for_width` +
/// `Backend::plugin_thumbnail` with a REAL approved image previewer): that
/// needs the same infrastructure as
/// `norte-core/tests/plugins_preview_image_e2e.rs` (compiling
/// `plugins/image-ansi` to `wasm32-wasip2`, installing, approving), which
/// does not exist today in `norte-tui/tests` — debt noted in the report,
/// not built in this round.
#[test]
fn un_previewer_de_plugin_no_esconde_que_los_bytes_son_imagen() {
    let png: &[u8] = b"\x89PNG\r\n\x1a\n";
    let con_previewer = norte_frontend::viewer::Viewer::with_plugin_preview_styled(
        vp("mem:///x.png"),
        "un-previewer".to_owned(),
        &[],
        false,
    );
    assert!(
        !con_previewer.is_image(),
        "is_image() sees the previewer, not the file"
    );
    assert!(
        norte_frontend::viewer::image_format(png).is_some(),
        "but the same file's bytes still say it IS an image"
    );
}

/// Task 4: the APC that places the image carries the id, the size in CELLS
/// (`c`/`r`, not pixels) and the bytes in base64 — never raw, because an
/// APC closes with `\x1b\\` and a PNG normally contains that byte pair.
#[test]
fn colocar_lleva_el_id_el_tamano_y_base64() {
    let esc = escape_colocar(7, b"PNGFALSO", Rect::new(1, 2, 40, 20), None);
    assert!(esc.starts_with("\x1b_G"), "starts with APC: {esc}");
    assert!(esc.contains("i=7"), "carries the id: {esc}");
    assert!(
        esc.contains("f=100"),
        "PNG: the only thing `imagen_desde_miniatura` lets through"
    );
    assert!(
        esc.contains("c=40") && esc.contains("r=20"),
        "the slot: {esc}"
    );
    // Fix round 2 (new MINOR): without `C=1` placing moves the cursor and
    // can scroll the screen (CRITICAL 1); without `q=2` the terminal
    // answers and its reply enters the event reader as loose keystrokes
    // (CRITICAL 3). Neither had an assertion stopping it from silently
    // disappearing.
    assert!(
        esc.contains("C=1"),
        "does not move the cursor when placing: {esc}"
    );
    assert!(esc.contains("q=2"), "silences the terminal's reply: {esc}");
    assert!(esc.ends_with("\x1b\\"), "closes the APC: {esc}");
    // The bytes go in base64 and NOT raw: an APC ends with `\x1b\\`, and a
    // PNG normally contains that byte pair.
    assert!(esc.contains("UE5HRkFMU08"), "base64 of the content: {esc}");
}

fn thumb(mimetype: &str) -> PluginThumbnail {
    PluginThumbnail {
        plugin_id: "image-thumb".to_owned(),
        plugin_name: "image-thumb".to_owned(),
        mimetype: mimetype.to_owned(),
        bytes: png_bytes_binarios(),
        width: 8,
        height: 4,
    }
}

/// FINDING 1 of the branch review: `escape_colocar` sends a FIXED `f=100`
/// — kitty has no `f=` key for JPEG or WebP, only PNG (100) or raw raster
/// (24/32) — but `PluginThumbnail.mimetype` allows all three, and
/// `thumb::reencode` (`norte-plugin-host`) really does fall back to JPEG
/// when the PNG does not fit in 4 MiB, easy with the `max_edge` of up to
/// 1920 px this viewer requests. A JPEG placed with a header that says PNG
/// gets rejected by kitty SILENTLY (`q=2`), and without this filter nobody
/// finds out: no trace, no retry, no notice.
#[test]
fn una_miniatura_jpeg_se_descarta() {
    let imagen = imagen_desde_miniatura(&vp("mem:///x.jpg"), thumb("image/jpeg"));
    assert!(
        matches!(imagen, Miniatura::FormatoAjeno),
        "kitty does not know f= for JPEG: it must be discarded, not placed \
         wrong — and discarded SAYING it was the format, not as a \"there is \
         none\": {imagen:?}"
    );
}

/// Same chain for WebP — the third format `plugin.thumbnail` can return and
/// that kitty also cannot place.
#[test]
fn una_miniatura_webp_se_descarta() {
    let imagen = imagen_desde_miniatura(&vp("mem:///x.webp"), thumb("image/webp"));
    assert!(
        matches!(imagen, Miniatura::FormatoAjeno),
        "kitty does not know f= for WebP: {imagen:?}"
    );
}

/// PNG, the only format kitty's protocol understands, DOES get placed —
/// and carries its mimetype along, so the invariant is checkable through
/// the rest of the path (not just documented).
#[test]
fn una_miniatura_png_se_coloca() {
    let path = vp("mem:///x.png");
    let imagen = imagen_desde_miniatura(&path, thumb("image/png"))
        .colocable()
        .expect("a PNG does get placed");
    assert_eq!(imagen.mimetype, "image/png");
    assert_eq!(imagen.path, path);
}

/// A real thumbnail does not fit in a single APC, so it has to be
/// chunked: every chunk but the last carries `m=1` and the last `m=0`.
/// Without this test, the ones above pass with an `escape_colocar` that
/// does not know how to chunk — 8 bytes never reach the cap.
#[test]
fn un_contenido_grande_se_trocea() {
    let grande = vec![0u8; 12 * 1024];
    let esc = escape_colocar(7, &grande, Rect::new(1, 2, 40, 20), None);
    let trozos: Vec<&str> = esc.split("\x1b_G").skip(1).collect();
    assert!(
        trozos.len() > 1,
        "a large image goes in several chunks: {}",
        trozos.len()
    );
    let (ultimo, previos) = trozos.split_last().expect("there is at least one");
    for t in previos {
        assert!(
            t.contains("m=1"),
            "a chunk that is not the last continues: {t}"
        );
    }
    assert!(ultimo.contains("m=0"), "the last one closes: {ultimo}");
}

/// `d=I` (uppercase) deletes the placement AND frees the bytes the
/// terminal keeps for the id — not just `d=i` (lowercase), which leaves the
/// PNG alive in the terminal's memory forever because `mint_image_id` never
/// recycles an id (fix round 1, IMPORTANT 6: this assertion was wrong in
/// the original brief, not the code that followed it). `i=<id>` keeps
/// bounding the deletion to THIS image: without it every image on the whole
/// terminal would be deleted, including another program's in another tab.
#[test]
fn borrar_nombra_solo_ese_id_y_libera_los_datos() {
    let esc = escape_borrar(7);
    assert!(
        esc.contains("a=d") && esc.contains("d=I") && esc.contains("i=7"),
        "{esc}"
    );
}

/// MINOR 8 (fix round 1): the rect the run loop sends the terminal
/// (`ui::rect_del_visor`) has to be the SAME one `draw_viewer` leaves blank
/// when there is a placed image — not two counts of the same slot that
/// could silently diverge (memory `funcion-compartida-no-basta`).
///
/// Without this test, `rect_del_visor` could return the frame WITH borders
/// (the CRITICAL 2 bug) and nothing would have caught it: the tests above
/// only look at the escape's SHAPE, never where it really lands. This one
/// really renders and checks the CELLS.
#[test]
fn el_rect_del_visor_es_el_hueco_que_draw_viewer_deja_en_blanco() {
    let dir = vp("mem:///");
    let mut app = app_en(&dir);
    let path = vp("mem:///x.png");
    app.viewer = Some(norte_tui::viewer::Viewer::new(
        path.clone(),
        b"\x89PNG\r\n\x1a\n".to_vec(),
        false,
    ));
    app.viewer_imagen = Some(ImagenColocada {
        path,
        bytes: vec![0u8; 4],
        mimetype: "image/png".to_owned(),
        width: 8,
        height: 4,
        id: 7,
        puesta_en: None,
    });
    let area = Rect::new(0, 0, 40, 12);
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(area.width, area.height))
            .expect("test terminal");
    terminal
        .draw(|f| norte_tui::ui::draw(f, &app))
        .expect("draw");
    let hueco = norte_tui::ui::rect_del_visor(&app, area);
    assert!(
        hueco.width > 0 && hueco.height > 0,
        "the slot cannot be empty on a {area:?} terminal: {hueco:?}"
    );
    let buf = terminal.backend().buffer();
    for y in hueco.top()..hueco.bottom() {
        for x in hueco.left()..hueco.right() {
            assert_eq!(
                buf[(x, y)].symbol(),
                " ",
                "cell ({x},{y}) of the slot should be blank with the image \
                 placed"
            );
        }
    }
    // And the frame, right ABOVE the slot, is NOT blank: if it were, the
    // test above would pass with any rect bigger than the real one —
    // exactly the CRITICAL 2 bug, which overran the borders and covered the
    // frame with `z=0`.
    let borde_y = hueco.top() - 1;
    let fila: String = (0..area.width)
        .map(|x| buf[(x, borde_y)].symbol().chars().next().unwrap_or(' '))
        .collect();
    assert!(
        fila.contains(['┌', '─', '┐']),
        "above the slot the viewer's frame is still there, not more blank: {fila:?}"
    );
}

fn app_con_imagen_colocada(area_no_vacia: bool) -> (App, Rect) {
    let dir = vp("mem:///");
    let mut app = app_en(&dir);
    let path = vp("mem:///x.png");
    app.viewer = Some(norte_tui::viewer::Viewer::new(
        path.clone(),
        png_bytes_binarios(),
        false,
    ));
    app.viewer_imagen = Some(ImagenColocada {
        path,
        bytes: vec![0u8; 4],
        mimetype: "image/png".to_owned(),
        width: 8,
        height: 4,
        id: 7,
        puesta_en: None,
    });
    let area = if area_no_vacia {
        Rect::new(0, 0, 40, 12)
    } else {
        Rect::new(0, 0, 0, 0)
    };
    (app, area)
}

/// FINDING 2 of the branch review: the painter (`draw_viewer`'s
/// `hay_imagen`) and the run loop (`coloca`, T4) looked at DIFFERENT
/// conditions to decide whether the thumbnail shows — the painter only the
/// `path`, the run loop also "nothing painted on top" and "the rect is not
/// empty". With an overlay that does NOT cover the whole screen (a small
/// modal, the menu, which-key) the painter blanked the slot as always while
/// the run loop refused to place pixels: neither image nor hexview. Both
/// questions are now the SAME function (`ui::imagen_a_colocar`).
#[test]
fn con_algo_encima_no_se_coloca_aunque_el_path_case() {
    let (mut app, area) = app_con_imagen_colocada(true);
    assert!(
        norte_tui::ui::imagen_a_colocar(&app, area).is_some(),
        "with nothing on top, the thumbnail is placed"
    );
    app.modal = Some(Modal::ConfirmQuit);
    assert!(
        norte_tui::ui::imagen_a_colocar(&app, area).is_none(),
        "a modal open over the viewer must not let pixels be placed"
    );
}

/// The empty slot (terminal too short to leave room for the frame and its
/// interior) is the other case where nothing gets placed — the empty-rect
/// test the branch review asked for separately from the overlay check
/// above: before this pass, `imagen_a_colocar` did not exist and nothing
/// tested this branch in isolation from the rest of `coloca`.
#[test]
fn con_el_rect_vacio_no_se_coloca() {
    let (app, area) = app_con_imagen_colocada(false);
    assert!(
        norte_tui::ui::imagen_a_colocar(&app, area).is_none(),
        "with no slot to land in, there is nothing to place: {area:?}"
    );
}

/// FINDING 3 of the branch review: the mode used to REQUEST the thumbnail
/// (`viewer_open::open_viewer`, resolved once on open) and the mode used to
/// WARN and PLACE (recalculated every frame against the current config)
/// could diverge on a hot reload — `[ui] images` reloads live
/// (`applies_live`). These two tests pin the method
/// `config_reload::reload_config` calls after reassigning `App::chrome`
/// (`reload_config` itself needs a `Backend`, three `Resolver`s and a
/// `Layers`, too much for a unit test of this).
#[test]
fn kitty_a_off_suelta_la_miniatura_colocada_ya_puesta() {
    let (mut app, _) = app_con_imagen_colocada(true);
    app.viewer_modo = Modo::Kitty;
    let soltada = app.soltar_miniatura_si_deja_de_ser_kitty(Modo::Nada);
    assert!(
        soltada,
        "a Kitty that switches to off must release the thumbnail"
    );
    assert!(
        app.viewer_imagen.is_none(),
        "without this the pixels stay stuck on screen forever, violating \
         what help promises about `off`"
    );
    assert_eq!(
        app.viewer_modo,
        Modo::Nada,
        "the pinned mode updates at the same time the thumbnail is released"
    );
}

/// The opposite direction (`Bloques`/`Nada` → `Kitty`) does NOT release or
/// update anything, on purpose: doing so would revive the review's other
/// hole — the "approve the thumbnail extension" notice would show over a
/// file the new mode NEVER requested one for.
#[test]
fn blocks_a_kitty_no_toca_el_modo_pineado() {
    let dir = vp("mem:///");
    let mut app = app_en(&dir);
    app.viewer = Some(norte_tui::viewer::Viewer::new(
        vp("mem:///x.png"),
        png_bytes_binarios(),
        false,
    ));
    app.viewer_modo = Modo::Bloques;
    let soltada = app.soltar_miniatura_si_deja_de_ser_kitty(Modo::Kitty);
    assert!(
        !soltada,
        "it only acts when the pinned mode was ALREADY Kitty"
    );
    assert_eq!(
        app.viewer_modo,
        Modo::Bloques,
        "it stays pinned until the reader reopens the file"
    );
}

/// End to end of the same finding: `panels::draw_viewer` has to read
/// `App::viewer_modo` (pinned) and NOT recompute against `app.chrome` LIVE.
/// It simulates exactly the scenario the review described — the reader
/// opened the PNG under `Bloques` (a thumbnail was never requested) and
/// THEN the config switched to `kitty` on the fly, without the reader
/// reopening anything — and it checks that the notice stays `Bloques`'s
/// ("preview"), not `Kitty`'s ("thumbnails") on a file that extension was
/// never asked for.
#[test]
fn el_pintor_usa_el_modo_pineado_no_el_chrome_en_vivo() {
    let _ = norte_i18n::force(norte_i18n::Lang::Es);
    let dir = vp("mem:///");
    let mut app = app_en(&dir);
    app.viewer = Some(norte_tui::viewer::Viewer::new(
        vp("mem:///x.png"),
        png_bytes_binarios(),
        false,
    ));
    // What `open_viewer` set when the reader opened the file.
    app.viewer_modo = Modo::Bloques;
    // What a hot reload changed AFTERWARD, without touching `viewer_modo`
    // (finding 3's own fix: only the Kitty→something-else direction
    // updates the pinned mode).
    app.chrome.images = Some(Images::Kitty);
    // With no `viewer_imagen`: a thumbnail was never requested under `Bloques`.

    let area = Rect::new(0, 0, 80, 16);
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(area.width, area.height))
            .expect("test terminal");
    terminal
        .draw(|f| norte_tui::ui::draw(f, &app))
        .expect("draw");
    let buf = terminal.backend().buffer();
    let status_y = area.height - 1;
    let fila: String = (0..area.width)
        .map(|x| buf[(x, status_y)].symbol().chars().next().unwrap_or(' '))
        .collect();
    assert!(
        fila.contains("vista previa"),
        "the notice must stay Bloques's, the mode it was opened under: {fila:?}"
    );
    assert!(
        !fila.contains("miniaturas"),
        "the Kitty notice would lie: a thumbnail was never requested for \
         this file: {fila:?}"
    );
}

/// With no viewer open there is nothing to release — a config change with
/// the browser (not the viewer) in front must not touch `viewer_imagen`,
/// which is already `None`.
#[test]
fn sin_visor_no_hay_nada_que_soltar() {
    let dir = vp("mem:///");
    let mut app = app_en(&dir);
    assert_eq!(app.viewer_modo, Modo::Nada);
    let soltada = app.soltar_miniatura_si_deja_de_ser_kitty(Modo::Nada);
    assert!(
        !soltada,
        "with no viewer open there is no thumbnail to release"
    );
}

/// Task 5: the usability hole the pilot found — with no previewer approved,
/// a PNG in `Modo::Bloques` falls to hexview just like a file nobody knows
/// how to interpret, and nothing on screen told the two cases apart.
#[test]
fn en_bloques_sin_previewer_el_visor_lo_dice() {
    // A silent hexview is indistinguishable from "norte does not know how".
    let aviso = aviso_de_imagen(Modo::Bloques, false, false);
    assert!(aviso.is_some(), "it has to say the plugin needs approving");
}

#[test]
fn con_previewer_no_se_avisa_de_nada() {
    assert!(aviso_de_imagen(Modo::Bloques, true, false).is_none());
}

#[test]
fn en_off_no_se_avisa_porque_lo_pidio_el_lector() {
    assert!(aviso_de_imagen(Modo::Nada, false, false).is_none());
}

/// Task 5b (finding from round 6's review): the same hole as Task 5, but
/// in `Modo::Kitty`. `aviso_de_imagen`'s docstring said that in Kitty "the
/// terminal already paints pixels on its own" and so there was nothing to
/// warn about — false: the pixels come from a `thumbnail` plugin
/// (`plugins/image-thumb`), just as approvable and absent by default as
/// `Modo::Bloques`'s previewer. With none approved, `Modo::Kitty` fell to
/// hexview as silently as the hole Task 5 covered on the other branch.
#[test]
fn en_kitty_sin_miniatura_el_visor_lo_dice() {
    let aviso = aviso_de_imagen(Modo::Kitty, false, false);
    assert!(
        aviso.is_some(),
        "it has to say the thumbnail plugin needs approving"
    );
}

#[test]
fn con_miniatura_colocada_no_se_avisa_de_nada_en_kitty() {
    assert!(aviso_de_imagen(Modo::Kitty, true, false).is_none());
}

/// The TWO reasons there are no pixels in Kitty are not fixed the same
/// way, and until now said the same thing: with a thumbnail extension
/// approved and enabled that answers in JPEG, the viewer sent the reader to
/// F12 to approve what was already approved. A notice that asks for the
/// impossible is worse than one that stays quiet.
#[test]
fn un_formato_que_kitty_no_coloca_no_manda_a_aprobar_nada() {
    let falta = aviso_de_imagen(Modo::Kitty, false, false).expect("with no plugin, it warns");
    let formato =
        aviso_de_imagen(Modo::Kitty, false, true).expect("with a foreign format, it warns");
    assert_ne!(
        falta, formato,
        "\"there is no extension\" and \"there is one and it answered in \
         another format\" are two different things"
    );
    assert!(
        !formato.contains("F12"),
        "there is nothing to approve at F12: the extension is already \
         approved — {formato}"
    );
}

/// And the foreign format only matters in Kitty: in blocks the pixels do
/// not go through the terminal's protocol, so a rejected thumbnail says
/// nothing on that branch.
#[test]
fn el_formato_ajeno_no_cambia_el_aviso_de_bloques() {
    assert_eq!(
        aviso_de_imagen(Modo::Bloques, false, true),
        aviso_de_imagen(Modo::Bloques, false, false)
    );
}

/// The two branches ask to approve DIFFERENT extensions (`previewer` vs
/// `thumbnail`): a notice that reused `Modo::Bloques`'s text in
/// `Modo::Kitty` would send the reader to approve the wrong one, which is
/// worse than not warning (the brief calls this out explicitly).
#[test]
fn el_aviso_de_bloques_y_el_de_kitty_son_textos_distintos() {
    let bloques = aviso_de_imagen(Modo::Bloques, false, false).expect("bloques warns");
    let kitty = aviso_de_imagen(Modo::Kitty, false, false).expect("kitty warns");
    assert_ne!(
        bloques, kitty,
        "each mode asks for a different extension to be approved"
    );
}

/// Counterpart of `no_hace_falta_avisar_de_imagen` for `Modo::Kitty`: it
/// uses the `thumbnail` plugin, not the `previewer`, so the "it is already
/// visible" condition is different — a thumbnail PLACED for THIS file, not
/// a previewer that replaced the raw view.
#[test]
fn no_hace_falta_avisar_de_miniatura_cuando_no_es_imagen() {
    use norte_tui::viewer_open::no_hace_falta_avisar_de_miniatura;
    let v = norte_tui::viewer::Viewer::new(vp("mem:///x.txt"), b"hola mundo".to_vec(), false);
    assert!(
        no_hace_falta_avisar_de_miniatura(&v, None),
        "it is not an image: nothing to warn about"
    );
}

#[test]
fn no_hace_falta_avisar_de_miniatura_cuando_ya_hay_una_colocada() {
    use norte_tui::viewer_open::no_hace_falta_avisar_de_miniatura;
    let path = vp("mem:///x.png");
    let v = norte_tui::viewer::Viewer::new(path.clone(), png_bytes_binarios(), false);
    let imagen = ImagenColocada {
        path,
        bytes: vec![0u8; 4],
        mimetype: "image/png".to_owned(),
        width: 8,
        height: 4,
        id: 1,
        puesta_en: None,
    };
    assert!(
        no_hace_falta_avisar_de_miniatura(&v, Some(&imagen)),
        "there are already pixels placed: nothing to warn about"
    );
}

/// The placed thumbnail is of ANOTHER file (the reader already moved, or
/// T4 has not replaced it yet): it is still necessary to warn about the one
/// showing NOW.
#[test]
fn no_hace_falta_avisar_de_miniatura_compara_el_path() {
    use norte_tui::viewer_open::no_hace_falta_avisar_de_miniatura;
    let v = norte_tui::viewer::Viewer::new(vp("mem:///x.png"), png_bytes_binarios(), false);
    let de_otro_fichero = ImagenColocada {
        path: vp("mem:///otro.png"),
        bytes: vec![0u8; 4],
        mimetype: "image/png".to_owned(),
        width: 8,
        height: 4,
        id: 1,
        puesta_en: None,
    };
    assert!(
        !no_hace_falta_avisar_de_miniatura(&v, Some(&de_otro_fichero)),
        "the placed thumbnail is of ANOTHER file: this one's is still missing"
    );
}

/// Half-blocks painted (a plugin previewer replaced the raw view, as in
/// `Modo::Bloques`) also turn off `is_image()`: if THAT is already visible,
/// there is nothing to warn about for the thumbnail plugin either, even
/// with no `ImagenColocada`.
#[test]
fn no_hace_falta_avisar_de_miniatura_cuando_un_previewer_ya_sustituyo_la_vista() {
    use norte_tui::viewer_open::no_hace_falta_avisar_de_miniatura;
    let v = norte_frontend::viewer::Viewer::with_plugin_preview_styled(
        vp("mem:///x.png"),
        "un-previewer".to_owned(),
        &[],
        false,
    );
    assert!(
        no_hace_falta_avisar_de_miniatura(&v, None),
        "a previewer already painted something: nothing to warn about for \
         the thumbnail plugin"
    );
}

/// Renders the app with a PNG in hexview (no previewer) and returns the
/// terminal's last row (the full-screen viewer's status bar,
/// `status_area`), at 80 columns — the task's reference width
/// (`snapshot_viewer_texto_y_hex` uses the same one).
///
/// Real PNG magic signature (`is_image()` recognizes it) + padding up to 40
/// bytes: at 16 bytes per hexview row (`HEX_COLS`) that gives 3 rows, so
/// `scroll_down(1)` leaves a position that is NOT the trivial "1/1".
fn fila_de_estado_con_png_sin_previewer() -> String {
    let dir = vp("mem:///");
    let mut app = app_en(&dir);
    // Finding 3: `panels::draw_viewer` reads `App::viewer_modo` (set on
    // open), not a live recalculation — `Auto` with no terminal probe
    // (there is no tty in a test) is `Modo::Bloques`
    // (`auto_usa_kitty_solo_si_el_terminal_sabe`), which is exactly what
    // `open_viewer` would have set here.
    app.viewer_modo = Modo::Bloques;
    let mut bytes = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR".to_vec();
    bytes.resize(40, 0);
    let mut v = norte_tui::viewer::Viewer::new(vp("mem:///x.png"), bytes, false);
    v.scroll_down(1);
    app.viewer = Some(v);

    let area = Rect::new(0, 0, 80, 16);
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(area.width, area.height))
            .expect("test terminal");
    terminal
        .draw(|f| norte_tui::ui::draw(f, &app))
        .expect("draw");
    let buf = terminal.backend().buffer();
    let status_y = area.height - 1;
    (0..area.width)
        .map(|x| buf[(x, status_y)].symbol().chars().next().unwrap_or(' '))
        .collect()
}

/// Fix round 1, IMPORTANT 2: with no previewer approved is the DEFAULT
/// state of any install (nothing to approve yet), so the notice branch is
/// the COMMON case, not the rare one. Before that fix,
/// `format!(" {aviso}")` replaced the whole status bar and ate `n/total` —
/// a large PNG in hexview lost the position count exactly while it was
/// being scrolled.
///
/// Fix round 2: round 1's `format!(" {aviso}  {pos}")` was correct in the
/// code but NOT on screen — the ORIGINAL text (82 characters) already
/// overflowed the 80 columns by itself, so `pos` stayed invisible.
/// `es.ftl`/`en.ftl` were shortened so both fit with `pos` next to them;
/// this test fixes the locale to ES (`norte_i18n::force`, only the
/// process's FIRST call wins — nextest gives one process per test, so it
/// does not clash with
/// `el_aviso_no_se_come_la_posicion_de_scroll_en_ingles` below, which fixes
/// EN in ANOTHER process) to test the case that was really broken, not the
/// one this machine's environment (`LANG=en_US.UTF-8`) happened to pass by
/// chance.
#[test]
fn el_aviso_no_se_come_la_posicion_de_scroll() {
    let _ = norte_i18n::force(norte_i18n::Lang::Es);
    let fila = fila_de_estado_con_png_sin_previewer();
    assert!(
        fila.contains("2/3"),
        "the notice must not eat the position, in ES: {fila:?}"
    );
    assert!(
        fila.contains("F12"),
        "and the notice is still present at the same time, in ES: {fila:?}"
    );
}

/// Same case as [`el_aviso_no_se_come_la_posicion_de_scroll`], in EN —
/// separate process under nextest, same reason to fix the locale.
#[test]
fn el_aviso_no_se_come_la_posicion_de_scroll_en_ingles() {
    let _ = norte_i18n::force(norte_i18n::Lang::En);
    let fila = fila_de_estado_con_png_sin_previewer();
    assert!(
        fila.contains("2/3"),
        "the notice must not eat the position, in EN: {fila:?}"
    );
    assert!(
        fila.contains("F12"),
        "and the notice is still present at the same time, in EN: {fila:?}"
    );
}

/// Task 5b: the same render as `fila_de_estado_con_png_sin_previewer`, but
/// forcing `images = "kitty"` (`Images::Kitty` rules the same with no probe,
/// see `kitty_forzado_manda_aunque_la_sonda_dijera_que_no`) and WITHOUT
/// placing `app.viewer_imagen` — the real case: `Modo::Kitty` requested the
/// thumbnail through the `thumbnail` plugin and none was approved, so
/// `viewer_for_width` returned `None` and nothing got placed.
fn fila_de_estado_con_png_en_kitty_sin_miniatura() -> String {
    let dir = vp("mem:///");
    let mut app = app_en(&dir);
    app.chrome.images = Some(Images::Kitty);
    // Finding 3: `panels::draw_viewer` reads `App::viewer_modo` (set on
    // open), not `app.chrome.images()` live — this test bypasses
    // `open_viewer`, so it has to set it itself, the way `open_viewer`
    // would for a viewer opened under this `chrome`.
    app.viewer_modo = Modo::Kitty;
    let mut bytes = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR".to_vec();
    bytes.resize(40, 0);
    let mut v = norte_tui::viewer::Viewer::new(vp("mem:///x.png"), bytes, false);
    v.scroll_down(1);
    app.viewer = Some(v);
    // On purpose `app.viewer_imagen` is NOT set: it is exactly the state
    // `viewer_for_width` leaves with no `thumbnail` plugin approved.

    let area = Rect::new(0, 0, 80, 16);
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(area.width, area.height))
            .expect("test terminal");
    terminal
        .draw(|f| norte_tui::ui::draw(f, &app))
        .expect("draw");
    let buf = terminal.backend().buffer();
    let status_y = area.height - 1;
    (0..area.width)
        .map(|x| buf[(x, status_y)].symbol().chars().next().unwrap_or(' '))
        .collect()
}

/// The defect T6's review reported: in `Modo::Kitty` with no thumbnail
/// plugin, the viewer stayed in hexview with NO notice at all — exactly the
/// hole Task 5 covered in `Modo::Bloques`, reopened on the other branch.
/// Same 80-column budget: the notice AND `pos` visible at the same time.
#[test]
fn en_kitty_sin_miniatura_el_visor_lo_dice_y_no_se_come_la_posicion() {
    let _ = norte_i18n::force(norte_i18n::Lang::Es);
    let fila = fila_de_estado_con_png_en_kitty_sin_miniatura();
    assert!(
        fila.contains("2/3"),
        "the notice must not eat the position, in ES: {fila:?}"
    );
    assert!(
        fila.contains("F12"),
        "and the thumbnail notice is still present at the same time, in ES: {fila:?}"
    );
}

/// Same case, in EN — separate process under nextest.
#[test]
fn en_kitty_sin_miniatura_el_visor_lo_dice_y_no_se_come_la_posicion_en_ingles() {
    let _ = norte_i18n::force(norte_i18n::Lang::En);
    let fila = fila_de_estado_con_png_en_kitty_sin_miniatura();
    assert!(
        fila.contains("2/3"),
        "the notice must not eat the position, in EN: {fila:?}"
    );
    assert!(
        fila.contains("F12"),
        "and the thumbnail notice is still present at the same time, in EN: {fila:?}"
    );
}

/// With the thumbnail ALREADY placed (pixels put down), Kitty's notice must
/// NOT show — "if the image is already showing... there is nothing to warn
/// about" (brief). The content slot goes blank (T4), but the status bar
/// stays the normal one (encoding/EOL/…), with no F12 text.
#[test]
fn en_kitty_con_miniatura_colocada_no_sale_el_aviso() {
    let _ = norte_i18n::force(norte_i18n::Lang::Es);
    let dir = vp("mem:///");
    let mut app = app_en(&dir);
    app.chrome.images = Some(Images::Kitty);
    // Same as before: set the pinned mode by hand, see the comment in
    // `fila_de_estado_con_png_en_kitty_sin_miniatura`.
    app.viewer_modo = Modo::Kitty;
    let path = vp("mem:///x.png");
    app.viewer = Some(norte_tui::viewer::Viewer::new(
        path.clone(),
        png_bytes_binarios(),
        false,
    ));
    app.viewer_imagen = Some(ImagenColocada {
        path,
        bytes: vec![0u8; 4],
        mimetype: "image/png".to_owned(),
        width: 8,
        height: 4,
        id: 1,
        puesta_en: None,
    });

    let area = Rect::new(0, 0, 80, 16);
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(area.width, area.height))
            .expect("test terminal");
    terminal
        .draw(|f| norte_tui::ui::draw(f, &app))
        .expect("draw");
    let buf = terminal.backend().buffer();
    let status_y = area.height - 1;
    let fila: String = (0..area.width)
        .map(|x| buf[(x, status_y)].symbol().chars().next().unwrap_or(' '))
        .collect();
    assert!(
        !fila.contains("F12"),
        "with pixels already placed there is nothing to warn about: {fila:?}"
    );
}
