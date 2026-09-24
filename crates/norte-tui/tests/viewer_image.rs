//! Task 3 (phase 5 WOW): the decision of which mode the viewer uses for an
//! image, resolved against `[ui] images` and what the kitty probe answered
//! — see `viewer_open::modo_effective` — and two invariants that fix round 1
//! left as a regression: `App.viewer_imagen` cannot stay dangling when the
//! viewer closes, and it is not decided by `Viewer::is_image()` (which a
//! plugin previewer turns off).

use norte_config::Images;
use norte_proto::VPath;
use norte_proto::methods::PluginThumbnail;
use norte_tui::app::{App, Modal, Pane};
use norte_tui::kitty_graphics::{escape_delete, escape_place};
use norte_tui::viewer_open::{
    ImagenPlaced, Modo, Thumbnail, image_notice, imagen_from_thumbnail, modo_effective,
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
/// these tests. Same pattern `status_row_with_png_without_previewer` already
/// used (signature + `IHDR` + zero padding up to 40 bytes).
fn png_bytes_binaries() -> Vec<u8> {
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
fn auto_uses_kitty_only_if_terminal_knows() {
    assert_eq!(modo_effective(Images::Auto, true), Modo::Kitty);
    assert_eq!(modo_effective(Images::Auto, false), Modo::Blocks);
}

#[test]
fn forced_kitty_wins_even_if_the_probe_said_no() {
    // The probe can be wrong — a multiplexer with passthrough, a terminal
    // that does not answer but knows — and forcing exists for that. If it
    // truly does not know, what you see is garbage on screen, and that is
    // why it is not the default value.
    assert_eq!(modo_effective(Images::Kitty, false), Modo::Kitty);
}

#[test]
fn blocks_does_not_use_kitty_even_if_the_terminal_knows() {
    assert_eq!(modo_effective(Images::Blocks, true), Modo::Blocks);
}

#[test]
fn off_paints_nothing_and_leaves_the_viewer_as_it_was() {
    assert_eq!(modo_effective(Images::Off, true), Modo::Nothing);
}

/// FINDING 1 of fix round 1: `Command::ViewerClose` set `app.viewer = None`
/// without touching `app.viewer_imagen`, so after viewing an image with a
/// thumbnail and closing the viewer, the previous image's thumbnail stayed
/// alive — dangling until T4 uses it to place/delete by id. `App::close_viewer`
/// clears both at once; this test pins that invariant directly on the
/// method, without going through `dispatch` (which needs a `Backend` this
/// test does not).
#[test]
fn closing_the_viewer_also_clears_its_thumbnail() {
    let dir = vp("mem:///");
    let mut app = app_en(&dir);
    app.viewer = Some(norte_tui::viewer::Viewer::new(
        vp("mem:///x.png"),
        b"\x89PNG\r\n\x1a\n".to_vec(),
        false,
    ));
    app.viewer_imagen = Some(ImagenPlaced {
        path: vp("mem:///x.png"),
        bytes: vec![0u8; 4],
        mimetype: "image/png".to_owned(),
        width: 8,
        height: 4,
        id: 7,
        placed_in: None,
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
fn a_plugin_previewer_does_not_hide_that_the_bytes_are_an_image() {
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
fn placing_carries_the_id_the_size_and_base64() {
    let esc = escape_place(7, b"PNGFALSO", Rect::new(1, 2, 40, 20), None);
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
        bytes: png_bytes_binaries(),
        width: 8,
        height: 4,
    }
}

/// FINDING 1 of the branch review: `escape_place` sends a FIXED `f=100`
/// — kitty has no `f=` key for JPEG or WebP, only PNG (100) or raw raster
/// (24/32) — but `PluginThumbnail.mimetype` allows all three, and
/// `thumb::reencode` (`norte-plugin-host`) really does fall back to JPEG
/// when the PNG does not fit in 4 MiB, easy with the `max_edge` of up to
/// 1920 px this viewer requests. A JPEG placed with a header that says PNG
/// gets rejected by kitty SILENTLY (`q=2`), and without this filter nobody
/// finds out: no trace, no retry, no notice.
#[test]
fn a_jpeg_thumbnail_is_discarded() {
    let imagen = imagen_from_thumbnail(&vp("mem:///x.jpg"), thumb("image/jpeg"));
    assert!(
        matches!(imagen, Thumbnail::FormatForeign),
        "kitty does not know f= for JPEG: it must be discarded, not placed \
         wrong — and discarded SAYING it was the format, not as a \"there is \
         none\": {imagen:?}"
    );
}

/// Same chain for WebP — the third format `plugin.thumbnail` can return and
/// that kitty also cannot place.
#[test]
fn a_webp_thumbnail_is_discarded() {
    let imagen = imagen_from_thumbnail(&vp("mem:///x.webp"), thumb("image/webp"));
    assert!(
        matches!(imagen, Thumbnail::FormatForeign),
        "kitty does not know f= for WebP: {imagen:?}"
    );
}

/// PNG, the only format kitty's protocol understands, DOES get placed —
/// and carries its mimetype along, so the invariant is checkable through
/// the rest of the path (not just documented).
#[test]
fn a_png_thumbnail_is_placed() {
    let path = vp("mem:///x.png");
    let imagen = imagen_from_thumbnail(&path, thumb("image/png"))
        .placeable()
        .expect("a PNG does get placed");
    assert_eq!(imagen.mimetype, "image/png");
    assert_eq!(imagen.path, path);
}

/// A real thumbnail does not fit in a single APC, so it has to be
/// chunked: every chunk but the last carries `m=1` and the last `m=0`.
/// Without this test, the ones above pass with an `escape_place` that
/// does not know how to chunk — 8 bytes never reach the cap.
#[test]
fn large_content_is_chunked() {
    let large = vec![0u8; 12 * 1024];
    let esc = escape_place(7, &large, Rect::new(1, 2, 40, 20), None);
    let chunks: Vec<&str> = esc.split("\x1b_G").skip(1).collect();
    assert!(
        chunks.len() > 1,
        "a large image goes in several chunks: {}",
        chunks.len()
    );
    let (last, previous) = chunks.split_last().expect("there is at least one");
    for t in previous {
        assert!(
            t.contains("m=1"),
            "a chunk that is not the last continues: {t}"
        );
    }
    assert!(last.contains("m=0"), "the last one closes: {last}");
}

/// `d=I` (uppercase) deletes the placement AND frees the bytes the
/// terminal keeps for the id — not just `d=i` (lowercase), which leaves the
/// PNG alive in the terminal's memory forever because `mint_image_id` never
/// recycles an id (fix round 1, IMPORTANT 6: this assertion was wrong in
/// the original brief, not the code that followed it). `i=<id>` keeps
/// bounding the deletion to THIS image: without it every image on the whole
/// terminal would be deleted, including another program's in another tab.
#[test]
fn deleting_names_only_that_id_and_frees_the_data() {
    let esc = escape_delete(7);
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
fn the_viewer_rect_is_the_slot_draw_viewer_leaves_blank() {
    let dir = vp("mem:///");
    let mut app = app_en(&dir);
    let path = vp("mem:///x.png");
    app.viewer = Some(norte_tui::viewer::Viewer::new(
        path.clone(),
        b"\x89PNG\r\n\x1a\n".to_vec(),
        false,
    ));
    app.viewer_imagen = Some(ImagenPlaced {
        path,
        bytes: vec![0u8; 4],
        mimetype: "image/png".to_owned(),
        width: 8,
        height: 4,
        id: 7,
        placed_in: None,
    });
    let area = Rect::new(0, 0, 40, 12);
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(area.width, area.height))
            .expect("test terminal");
    terminal
        .draw(|f| norte_tui::ui::draw(f, &app))
        .expect("draw");
    let slot = norte_tui::ui::rect_del_visor(&app, area);
    assert!(
        slot.width > 0 && slot.height > 0,
        "the slot cannot be empty on a {area:?} terminal: {slot:?}"
    );
    let buf = terminal.backend().buffer();
    for y in slot.top()..slot.bottom() {
        for x in slot.left()..slot.right() {
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
    let border_and = slot.top() - 1;
    let row: String = (0..area.width)
        .map(|x| buf[(x, border_and)].symbol().chars().next().unwrap_or(' '))
        .collect();
    assert!(
        row.contains(['┌', '─', '┐']),
        "above the slot the viewer's frame is still there, not more blank: {row:?}"
    );
}

fn app_with_placed_image(area_no_empty: bool) -> (App, Rect) {
    let dir = vp("mem:///");
    let mut app = app_en(&dir);
    let path = vp("mem:///x.png");
    app.viewer = Some(norte_tui::viewer::Viewer::new(
        path.clone(),
        png_bytes_binaries(),
        false,
    ));
    app.viewer_imagen = Some(ImagenPlaced {
        path,
        bytes: vec![0u8; 4],
        mimetype: "image/png".to_owned(),
        width: 8,
        height: 4,
        id: 7,
        placed_in: None,
    });
    let area = if area_no_empty {
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
/// questions are now the SAME function (`ui::image_to_place`).
#[test]
fn with_something_on_top_it_is_not_placed_even_though_the_path_matches() {
    let (mut app, area) = app_with_placed_image(true);
    assert!(
        norte_tui::ui::image_to_place(&app, area).is_some(),
        "with nothing on top, the thumbnail is placed"
    );
    app.modal = Some(Modal::ConfirmQuit);
    assert!(
        norte_tui::ui::image_to_place(&app, area).is_none(),
        "a modal open over the viewer must not let pixels be placed"
    );
}

/// The empty slot (terminal too short to leave room for the frame and its
/// interior) is the other case where nothing gets placed — the empty-rect
/// test the branch review asked for separately from the overlay check
/// above: before this pass, `image_to_place` did not exist and nothing
/// tested this branch in isolation from the rest of `coloca`.
#[test]
fn with_an_empty_rect_it_is_not_placed() {
    let (app, area) = app_with_placed_image(false);
    assert!(
        norte_tui::ui::image_to_place(&app, area).is_none(),
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
fn kitty_to_off_releases_the_already_placed_thumbnail() {
    let (mut app, _) = app_with_placed_image(true);
    app.viewer_modo = Modo::Kitty;
    let released = app.drop_thumbnail_if_no_longer_kitty(Modo::Nothing);
    assert!(
        released,
        "a Kitty that switches to off must release the thumbnail"
    );
    assert!(
        app.viewer_imagen.is_none(),
        "without this the pixels stay stuck on screen forever, violating \
         what help promises about `off`"
    );
    assert_eq!(
        app.viewer_modo,
        Modo::Nothing,
        "the pinned mode updates at the same time the thumbnail is released"
    );
}

/// The opposite direction (`Blocks`/`Nothing` → `Kitty`) does NOT release or
/// update anything, on purpose: doing so would revive the review's other
/// hole — the "approve the thumbnail extension" notice would show over a
/// file the new mode NEVER requested one for.
#[test]
fn blocks_to_kitty_does_not_touch_pinned_mode() {
    let dir = vp("mem:///");
    let mut app = app_en(&dir);
    app.viewer = Some(norte_tui::viewer::Viewer::new(
        vp("mem:///x.png"),
        png_bytes_binaries(),
        false,
    ));
    app.viewer_modo = Modo::Blocks;
    let released = app.drop_thumbnail_if_no_longer_kitty(Modo::Kitty);
    assert!(
        !released,
        "it only acts when the pinned mode was ALREADY Kitty"
    );
    assert_eq!(
        app.viewer_modo,
        Modo::Blocks,
        "it stays pinned until the reader reopens the file"
    );
}

/// End to end of the same finding: `panels::draw_viewer` has to read
/// `App::viewer_modo` (pinned) and NOT recompute against `app.chrome` LIVE.
/// It simulates exactly the scenario the review described — the reader
/// opened the PNG under `Blocks` (a thumbnail was never requested) and
/// THEN the config switched to `kitty` on the fly, without the reader
/// reopening anything — and it checks that the notice stays `Blocks`'s
/// ("preview"), not `Kitty`'s ("thumbnails") on a file that extension was
/// never asked for.
#[test]
fn the_painter_uses_the_pinned_mode_not_the_live_chrome() {
    let _ = norte_i18n::force(norte_i18n::Lang::Es);
    let dir = vp("mem:///");
    let mut app = app_en(&dir);
    app.viewer = Some(norte_tui::viewer::Viewer::new(
        vp("mem:///x.png"),
        png_bytes_binaries(),
        false,
    ));
    // What `open_viewer` set when the reader opened the file.
    app.viewer_modo = Modo::Blocks;
    // What a hot reload changed AFTERWARD, without touching `viewer_modo`
    // (finding 3's own fix: only the Kitty→something-else direction
    // updates the pinned mode).
    app.chrome.images = Some(Images::Kitty);
    // With no `viewer_imagen`: a thumbnail was never requested under `Blocks`.

    let area = Rect::new(0, 0, 80, 16);
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(area.width, area.height))
            .expect("test terminal");
    terminal
        .draw(|f| norte_tui::ui::draw(f, &app))
        .expect("draw");
    let buf = terminal.backend().buffer();
    let status_y = area.height - 1;
    let row: String = (0..area.width)
        .map(|x| buf[(x, status_y)].symbol().chars().next().unwrap_or(' '))
        .collect();
    assert!(
        row.contains("vista previa"),
        "the notice must stay Bloques's, the mode it was opened under: {row:?}"
    );
    assert!(
        !row.contains("miniaturas"),
        "the Kitty notice would lie: a thumbnail was never requested for \
         this file: {row:?}"
    );
}

/// With no viewer open there is nothing to release — a config change with
/// the browser (not the viewer) in front must not touch `viewer_imagen`,
/// which is already `None`.
#[test]
fn without_a_viewer_there_is_nothing_to_drop() {
    let dir = vp("mem:///");
    let mut app = app_en(&dir);
    assert_eq!(app.viewer_modo, Modo::Nothing);
    let released = app.drop_thumbnail_if_no_longer_kitty(Modo::Nothing);
    assert!(
        !released,
        "with no viewer open there is no thumbnail to release"
    );
}

/// Task 5: the usability hole the pilot found — with no previewer approved,
/// a PNG in `Modo::Blocks` falls to hexview just like a file nobody knows
/// how to interpret, and nothing on screen told the two cases apart.
#[test]
fn in_blocks_without_previewer_the_viewer_says_so() {
    // A silent hexview is indistinguishable from "norte does not know how".
    let notice = image_notice(Modo::Blocks, false, false);
    assert!(notice.is_some(), "it has to say the plugin needs approving");
}

#[test]
fn with_previewer_nothing_is_warned() {
    assert!(image_notice(Modo::Blocks, true, false).is_none());
}

#[test]
fn in_off_mode_there_is_no_warning_because_the_reader_asked_for_it() {
    assert!(image_notice(Modo::Nothing, false, false).is_none());
}

/// Task 5b (finding from round 6's review): the same hole as Task 5, but
/// in `Modo::Kitty`. `image_notice`'s docstring said that in Kitty "the
/// terminal already paints pixels on its own" and so there was nothing to
/// warn about — false: the pixels come from a `thumbnail` plugin
/// (`plugins/image-thumb`), just as approvable and absent by default as
/// `Modo::Blocks`'s previewer. With none approved, `Modo::Kitty` fell to
/// hexview as silently as the hole Task 5 covered on the other branch.
#[test]
fn in_kitty_without_thumbnail_the_viewer_says_so() {
    let notice = image_notice(Modo::Kitty, false, false);
    assert!(
        notice.is_some(),
        "it has to say the thumbnail plugin needs approving"
    );
}

#[test]
fn with_thumbnail_placed_nothing_is_warned_in_kitty() {
    assert!(image_notice(Modo::Kitty, true, false).is_none());
}

/// The TWO reasons there are no pixels in Kitty are not fixed the same
/// way, and until now said the same thing: with a thumbnail extension
/// approved and enabled that answers in JPEG, the viewer sent the reader to
/// F12 to approve what was already approved. A notice that asks for the
/// impossible is worse than one that stays quiet.
#[test]
fn a_format_kitty_cannot_place_does_not_send_anything_for_approval() {
    let missing = image_notice(Modo::Kitty, false, false).expect("with no plugin, it warns");
    let format = image_notice(Modo::Kitty, false, true).expect("with a foreign format, it warns");
    assert_ne!(
        missing, format,
        "\"there is no extension\" and \"there is one and it answered in \
         another format\" are two different things"
    );
    assert!(
        !format.contains("F12"),
        "there is nothing to approve at F12: the extension is already \
         approved — {format}"
    );
}

/// And the foreign format only matters in Kitty: in blocks the pixels do
/// not go through the terminal's protocol, so a rejected thumbnail says
/// nothing on that branch.
#[test]
fn a_foreign_format_does_not_change_the_blocks_warning() {
    assert_eq!(
        image_notice(Modo::Blocks, false, true),
        image_notice(Modo::Blocks, false, false)
    );
}

/// The two branches ask to approve DIFFERENT extensions (`previewer` vs
/// `thumbnail`): a notice that reused `Modo::Blocks`'s text in
/// `Modo::Kitty` would send the reader to approve the wrong one, which is
/// worse than not warning (the brief calls this out explicitly).
#[test]
fn the_blocks_warning_and_the_kitty_one_are_different_texts() {
    let blocks = image_notice(Modo::Blocks, false, false).expect("bloques warns");
    let kitty = image_notice(Modo::Kitty, false, false).expect("kitty warns");
    assert_ne!(
        blocks, kitty,
        "each mode asks for a different extension to be approved"
    );
}

/// Counterpart of `no_need_to_warn_about_image` for `Modo::Kitty`: it
/// uses the `thumbnail` plugin, not the `previewer`, so the "it is already
/// visible" condition is different — a thumbnail PLACED for THIS file, not
/// a previewer that replaced the raw view.
#[test]
fn no_thumbnail_notice_needed_when_it_is_not_an_image() {
    use norte_tui::viewer_open::no_need_to_warn_about_thumbnail;
    let v = norte_tui::viewer::Viewer::new(vp("mem:///x.txt"), b"hola mundo".to_vec(), false);
    assert!(
        no_need_to_warn_about_thumbnail(&v, None),
        "it is not an image: nothing to warn about"
    );
}

#[test]
fn no_thumbnail_notice_needed_when_one_is_already_placed() {
    use norte_tui::viewer_open::no_need_to_warn_about_thumbnail;
    let path = vp("mem:///x.png");
    let v = norte_tui::viewer::Viewer::new(path.clone(), png_bytes_binaries(), false);
    let imagen = ImagenPlaced {
        path,
        bytes: vec![0u8; 4],
        mimetype: "image/png".to_owned(),
        width: 8,
        height: 4,
        id: 1,
        placed_in: None,
    };
    assert!(
        no_need_to_warn_about_thumbnail(&v, Some(&imagen)),
        "there are already pixels placed: nothing to warn about"
    );
}

/// The placed thumbnail is of ANOTHER file (the reader already moved, or
/// T4 has not replaced it yet): it is still necessary to warn about the one
/// showing NOW.
#[test]
fn no_thumbnail_notice_needed_compares_the_path() {
    use norte_tui::viewer_open::no_need_to_warn_about_thumbnail;
    let v = norte_tui::viewer::Viewer::new(vp("mem:///x.png"), png_bytes_binaries(), false);
    let from_another_file = ImagenPlaced {
        path: vp("mem:///otro.png"),
        bytes: vec![0u8; 4],
        mimetype: "image/png".to_owned(),
        width: 8,
        height: 4,
        id: 1,
        placed_in: None,
    };
    assert!(
        !no_need_to_warn_about_thumbnail(&v, Some(&from_another_file)),
        "the placed thumbnail is of ANOTHER file: this one's is still missing"
    );
}

/// Half-blocks painted (a plugin previewer replaced the raw view, as in
/// `Modo::Blocks`) also turn off `is_image()`: if THAT is already visible,
/// there is nothing to warn about for the thumbnail plugin either, even
/// with no `ImagenPlaced`.
#[test]
fn no_thumbnail_notice_needed_when_a_previewer_already_replaced_the_view() {
    use norte_tui::viewer_open::no_need_to_warn_about_thumbnail;
    let v = norte_frontend::viewer::Viewer::with_plugin_preview_styled(
        vp("mem:///x.png"),
        "un-previewer".to_owned(),
        &[],
        false,
    );
    assert!(
        no_need_to_warn_about_thumbnail(&v, None),
        "a previewer already painted something: nothing to warn about for \
         the thumbnail plugin"
    );
}

/// Renders the app with a PNG in hexview (no previewer) and returns the
/// terminal's last row (the full-screen viewer's status bar,
/// `status_area`), at 80 columns — the task's reference width
/// (`snapshot_viewer_text_and_hex` uses the same one).
///
/// Real PNG magic signature (`is_image()` recognizes it) + padding up to 40
/// bytes: at 16 bytes per hexview row (`HEX_COLS`) that gives 3 rows, so
/// `scroll_down(1)` leaves a position that is NOT the trivial "1/1".
fn status_row_with_png_without_previewer() -> String {
    let dir = vp("mem:///");
    let mut app = app_en(&dir);
    // Finding 3: `panels::draw_viewer` reads `App::viewer_modo` (set on
    // open), not a live recalculation — `Auto` with no terminal probe
    // (there is no tty in a test) is `Modo::Blocks`
    // (`auto_uses_kitty_only_if_terminal_knows`), which is exactly what
    // `open_viewer` would have set here.
    app.viewer_modo = Modo::Blocks;
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
/// `format!(" {notice}")` replaced the whole status bar and ate `n/total` —
/// a large PNG in hexview lost the position count exactly while it was
/// being scrolled.
///
/// Fix round 2: round 1's `format!(" {notice}  {pos}")` was correct in the
/// code but NOT on screen — the ORIGINAL text (82 characters) already
/// overflowed the 80 columns by itself, so `pos` stayed invisible.
/// `es.ftl`/`en.ftl` were shortened so both fit with `pos` next to them;
/// this test fixes the locale to ES (`norte_i18n::force`, only the
/// process's FIRST call wins — nextest gives one process per test, so it
/// does not clash with
/// `the_warning_does_not_eat_the_scroll_position_in_english` below, which fixes
/// EN in ANOTHER process) to test the case that was really broken, not the
/// one this machine's environment (`LANG=en_US.UTF-8`) happened to pass by
/// chance.
#[test]
fn the_warning_does_not_eat_the_scroll_position() {
    let _ = norte_i18n::force(norte_i18n::Lang::Es);
    let row = status_row_with_png_without_previewer();
    assert!(
        row.contains("2/3"),
        "the notice must not eat the position, in ES: {row:?}"
    );
    assert!(
        row.contains("F12"),
        "and the notice is still present at the same time, in ES: {row:?}"
    );
}

/// Same case as [`the_warning_does_not_eat_the_scroll_position`], in EN —
/// separate process under nextest, same reason to fix the locale.
#[test]
fn the_warning_does_not_eat_the_scroll_position_in_english() {
    let _ = norte_i18n::force(norte_i18n::Lang::En);
    let row = status_row_with_png_without_previewer();
    assert!(
        row.contains("2/3"),
        "the notice must not eat the position, in EN: {row:?}"
    );
    assert!(
        row.contains("F12"),
        "and the notice is still present at the same time, in EN: {row:?}"
    );
}

/// Task 5b: the same render as `status_row_with_png_without_previewer`, but
/// forcing `images = "kitty"` (`Images::Kitty` rules the same with no probe,
/// see `forced_kitty_wins_even_if_the_probe_said_no`) and WITHOUT
/// placing `app.viewer_imagen` — the real case: `Modo::Kitty` requested the
/// thumbnail through the `thumbnail` plugin and none was approved, so
/// `viewer_for_width` returned `None` and nothing got placed.
fn status_row_with_png_in_kitty_without_thumbnail() -> String {
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
/// hole Task 5 covered in `Modo::Blocks`, reopened on the other branch.
/// Same 80-column budget: the notice AND `pos` visible at the same time.
#[test]
fn in_kitty_without_thumbnail_the_viewer_says_so_and_does_not_eat_the_position() {
    let _ = norte_i18n::force(norte_i18n::Lang::Es);
    let row = status_row_with_png_in_kitty_without_thumbnail();
    assert!(
        row.contains("2/3"),
        "the notice must not eat the position, in ES: {row:?}"
    );
    assert!(
        row.contains("F12"),
        "and the thumbnail notice is still present at the same time, in ES: {row:?}"
    );
}

/// Same case, in EN — separate process under nextest.
#[test]
fn in_kitty_without_thumbnail_the_viewer_says_so_and_does_not_eat_the_position_in_english() {
    let _ = norte_i18n::force(norte_i18n::Lang::En);
    let row = status_row_with_png_in_kitty_without_thumbnail();
    assert!(
        row.contains("2/3"),
        "the notice must not eat the position, in EN: {row:?}"
    );
    assert!(
        row.contains("F12"),
        "and the thumbnail notice is still present at the same time, in EN: {row:?}"
    );
}

/// With the thumbnail ALREADY placed (pixels put down), Kitty's notice must
/// NOT show — "if the image is already showing... there is nothing to warn
/// about" (brief). The content slot goes blank (T4), but the status bar
/// stays the normal one (encoding/EOL/…), with no F12 text.
#[test]
fn in_kitty_with_thumbnail_placed_the_warning_does_not_appear() {
    let _ = norte_i18n::force(norte_i18n::Lang::Es);
    let dir = vp("mem:///");
    let mut app = app_en(&dir);
    app.chrome.images = Some(Images::Kitty);
    // Same as before: set the pinned mode by hand, see the comment in
    // `status_row_with_png_in_kitty_without_thumbnail`.
    app.viewer_modo = Modo::Kitty;
    let path = vp("mem:///x.png");
    app.viewer = Some(norte_tui::viewer::Viewer::new(
        path.clone(),
        png_bytes_binaries(),
        false,
    ));
    app.viewer_imagen = Some(ImagenPlaced {
        path,
        bytes: vec![0u8; 4],
        mimetype: "image/png".to_owned(),
        width: 8,
        height: 4,
        id: 1,
        placed_in: None,
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
    let row: String = (0..area.width)
        .map(|x| buf[(x, status_y)].symbol().chars().next().unwrap_or(' '))
        .collect();
    assert!(
        !row.contains("F12"),
        "with pixels already placed there is nothing to warn about: {row:?}"
    );
}
