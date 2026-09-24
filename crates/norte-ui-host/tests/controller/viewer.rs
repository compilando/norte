use super::*;

// ---------------------------------------------------------------------------
// The viewer (phase 4, task 4.3).
// ---------------------------------------------------------------------------

/// Bytes `norte_encoding::detect` classifies as BINARY and that start with
/// the PNG signature: the signature alone is eight bytes with no NUL at all
/// and the heuristic takes them for text, which would make `is_image()` come
/// out `false`. Same mold as the TUI's `png_bytes_binarios`.
fn png_binary() -> Vec<u8> {
    let mut bytes = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR".to_vec();
    bytes.resize(40, 0);
    bytes
}

/// **Flipping through images flips through images, and the cursor comes with
/// you.**
///
/// The listing is sorted, so `b.txt` lands BETWEEN the two images: it is
/// exactly the row `viewer.next` has to skip over. And at the end of the
/// reel it says there are no more instead of wrapping back to the first.
#[tokio::test]
async fn next_sibling_skips_the_text_in_the_middle() {
    let mut f = Fake::default();
    f.put(
        "mem:///casa",
        vec![
            (b"a.png".to_vec(), false),
            (b"b.txt".to_vec(), false),
            (b"c.png".to_vec(), false),
        ],
    );
    f.content
        .insert("mem:///casa/a.png".to_owned(), png_binary());
    f.content
        .insert("mem:///casa/c.png".to_owned(), png_binary());
    f.content
        .insert("mem:///casa/b.txt".to_owned(), b"texto\n".to_vec());
    let (h, _snap) = host_tree(Arc::new(f)).await;
    let mut sub = h.subscribe();

    h.dispatch(press("F3")).await.expect("host alive");
    let v = next_visor(&mut sub).await.expect("the viewer opens");
    assert!(v.path_display.ends_with("a.png"), "opens the first image");

    // F9 = `viewer.next`: the NEXT image, not the text in the middle.
    h.dispatch(press("F9")).await.expect("host alive");
    let v = next_visor(&mut sub)
        .await
        .expect("the viewer opens another");
    assert!(
        v.path_display.ends_with("c.png"),
        "skips b.txt: {}",
        v.path_display
    );

    // And from the last one there are no more: it does not wrap to the
    // first.
    let ack = h.dispatch(press("F9")).await.expect("host alive");
    assert!(
        matches!(&ack, ActionAck::Unavailable { reason_key } if reason_key == "host-no-sibling"),
        "at the end of the reel it says there are no more: {ack:?}"
    );

    // F8 goes back, with the same rule.
    h.dispatch(press("F8")).await.expect("host alive");
    let v = next_visor(&mut sub).await.expect("the viewer goes back");
    assert!(
        v.path_display.ends_with("a.png"),
        "going backward also skips the text: {}",
        v.path_display
    );
}

/// F3 on a file OPENS it: a bounded header is read, decoded with the shared
/// detection, and what travels are already-sanitized lines.
#[tokio::test]
async fn viewing_a_file_decodes_it_and_paints_it() {
    let mut f = Fake::default();
    f.put("mem:///casa", vec![(b"notas.txt".to_vec(), false)]);
    f.content.insert(
        "mem:///casa/notas.txt".to_owned(),
        b"primera\nsegunda\ntercera\n".to_vec(),
    );
    let backend = Arc::new(f);
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    h.dispatch(press("F3")).await.expect("host alive");
    let v = next_visor(&mut sub).await.expect("the viewer is open");
    assert!(v.path_display.ends_with("notas.txt"));
    assert_eq!(v.total_rows, 3, "three lines");
    assert!(
        v.lines.iter().any(|l| l == "primera"),
        "and the text arrives decoded: {:?}",
        v.lines
    );
    assert!(!v.hex, "text is not shown in hexadecimal");
    assert_eq!(v.encoding.to_ascii_uppercase(), "UTF-8");
}

/// With the viewer open, the keys are ITS OWN: the same `viewer` screen map
/// the TUI uses, not a second keymap written here.
#[tokio::test]
async fn with_the_viewer_open_the_keys_belong_to_the_viewer() {
    let mut f = Fake::default();
    f.put("mem:///casa", vec![(b"notas.txt".to_vec(), false)]);
    let mut body = String::new();
    for i in 0..200 {
        use std::fmt::Write as _;
        let _ = writeln!(body, "linea {i}");
    }
    let body = body.into_bytes();
    f.content.insert("mem:///casa/notas.txt".to_owned(), body);
    let (h, _snap) = host_tree(Arc::new(f)).await;
    let mut sub = h.subscribe();
    h.dispatch(press("F3")).await.expect("host alive");
    let opened = next_visor(&mut sub).await.expect("viewer open");
    assert_eq!(opened.first_line, 0);

    // `down` in the viewer SCROLLS the viewer; it does not move the
    // listing's cursor.
    h.dispatch(press("ArrowDown")).await.expect("host alive");
    let scrolled = next_visor(&mut sub).await.expect("viewer open");
    assert_eq!(scrolled.first_line, 1);

    // And `esc` closes it.
    h.dispatch(press("Escape")).await.expect("host alive");
    assert!(next_visor(&mut sub).await.is_none(), "the viewer closes");

    // The listing underneath did not move, and seeing that takes a
    // snapshot: the viewer travels in patches precisely so it does not send
    // one on every keystroke.
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let snap = next_snapshot(&mut sub).await;
    assert_eq!(listing(&snap).cursor, Some(RowKey(0)));
}

/// A binary is not painted as if it were text: it shows in hexadecimal, and
/// the shared layer decides by CONTENT, not by extension.
#[tokio::test]
async fn a_binary_shows_in_hexadecimal() {
    let mut f = Fake::default();
    f.put("mem:///casa", vec![(b"raro.txt".to_vec(), false)]);
    f.content.insert(
        "mem:///casa/raro.txt".to_owned(),
        vec![0x00, 0x01, 0x02, 0xff, 0xfe, 0x00, 0x03],
    );
    let (h, _snap) = host_tree(Arc::new(f)).await;
    let mut sub = h.subscribe();
    h.dispatch(press("F3")).await.expect("host alive");
    let snap = next_snapshot(&mut sub).await;
    let v = snap.viewer.as_ref().expect("viewer");
    assert!(
        v.hex,
        "a binary goes into hexadecimal even if it is called .txt"
    );
}

// ---------------------------------------------------------------------------
// A plugin's preview in the viewer (task 4.3).
// ---------------------------------------------------------------------------

/// A styled preview, the way a previewer would return it.
pub(super) fn preview_de(
    plugin: &str,
    lines: &[&str],
    lossy: bool,
) -> norte_proto::methods::PluginPreviewStyled {
    norte_proto::methods::PluginPreviewStyled {
        plugin_id: "acme.pdf".to_owned(),
        plugin_name: plugin.to_owned(),
        lines: lines
            .iter()
            .map(|l| {
                vec![norte_proto::methods::SpanWire {
                    text: (*l).to_owned(),
                    role: Some("info".to_owned()),
                    fg: None,
                    bg: None,
                }]
            })
            .collect(),
        lossy,
    }
}

/// ADR 0141: an image the window paints ON ITS OWN goes through no plugin.
/// It used to be that, with an image previewer installed, its styled view
/// (ANSI art) won, the window ended up with no image of its own, and it also
/// requested the thumbnail: two plugins compiled in just to open a photo.
#[tokio::test]
async fn a_native_image_does_not_ask_the_plugins() {
    let mut f = Fake::default();
    f.put("mem:///casa", vec![(b"foto.png".to_vec(), false)]);
    f.content
        .insert("mem:///casa/foto.png".to_owned(), png_de(640, 480, 4096));
    // A previewer that would match: it must not be asked.
    f.previews.insert(
        "mem:///casa/foto.png".to_owned(),
        preview_de("Imagen ANSI", &["▀▀▀"], false),
    );
    let f = Arc::new(f);
    let (h, _snap) = host_tree(Arc::clone(&f)).await;
    let mut sub = h.subscribe();
    h.dispatch(press("F3")).await.expect("host alive");
    let mut v = None;
    for _ in 0..20 {
        h.dispatch(UiAction::Resync).await.expect("host alive");
        if let Some(x) = next_snapshot(&mut sub).await.viewer.clone() {
            v = Some(x);
            break;
        }
    }
    let v = v.expect("the viewer opens");
    assert!(v.image.is_some(), "the window paints its own");
    assert!(
        v.preview_by.is_empty(),
        "no plugin view: {:?}",
        v.preview_by
    );
    // A few more rounds so a late style, had it been requested, would have
    // had time to arrive.
    for _ in 0..5 {
        h.dispatch(UiAction::Resync).await.expect("host alive");
        let _ = next_snapshot(&mut sub).await;
    }
    assert!(
        f.anchos_de_preview.lock().expect("mutex").is_empty(),
        "no previewer was asked"
    );
}

/// When a previewer applies, the viewer shows ITS OWN and says whose it is.
///
/// A plugin can show anything — that is its job: a PDF as text, formatted
/// JSON — so whoever is looking has the right to know they are not seeing
/// the file's own bytes.
#[tokio::test]
async fn the_viewer_shows_a_plugins_preview_and_says_whose_it_is() {
    let mut f = Fake::default();
    f.put("mem:///casa", vec![(b"informe.pdf".to_vec(), false)]);
    f.content.insert(
        "mem:///casa/informe.pdf".to_owned(),
        b"%PDF-1.7 crudo".to_vec(),
    );
    f.previews.insert(
        "mem:///casa/informe.pdf".to_owned(),
        preview_de("PDF de ACME", &["Annual report", "Page 1 of 12"], true),
    );
    let (h, _snap) = host_tree(Arc::new(f)).await;
    let mut sub = h.subscribe();

    h.dispatch(press("F3")).await.expect("host alive");
    // The plugin's view arrives AFTER opening (ADR 0141): the viewer opens
    // with the raw one and this replaces it. It is waited for.
    let mut viewer = None;
    for _ in 0..40 {
        h.dispatch(UiAction::Resync).await.expect("host alive");
        if let Some(v) = next_snapshot(&mut sub).await.viewer.clone()
            && !v.preview_by.is_empty()
        {
            viewer = Some(v);
            break;
        }
    }
    let v = viewer.expect("the viewer opens with the plugin's view");

    assert!(
        v.lines.iter().any(|l| l.contains("Annual report")),
        "shows the previewer's: {:?}",
        v.lines
    );
    assert!(
        !v.lines.iter().any(|l| l.contains("%PDF")),
        "and NOT the raw bytes: showing both would be the same file twice — \
         {:?}",
        v.lines
    );
    assert!(
        v.preview_by.contains("PDF de ACME"),
        "and says whose it is: {:?}",
        v.preview_by
    );
    assert!(
        !v.preview_by.starts_with("viewer-plugin"),
        "translated, not the key: {:?}",
        v.preview_by
    );
    assert!(
        v.preview_lossy,
        "and that the decoding it was given was lossy: the `?` in its \
         output come from that, not from the file"
    );
}

/// A plugin's preview reaches the window WITH its fragments (bridge 49):
/// theme role in kebab case, own color in `#rrggbb`, text already masked.
/// Until now `ViewerView` flattened it to `lines`, and the window painted in
/// gray what the TUI painted in color.
#[tokio::test]
async fn the_viewer_carries_the_previews_fragments() {
    let mut f = Fake::default();
    f.put("mem:///casa", vec![(b"main.rs".to_vec(), false)]);
    f.content
        .insert("mem:///casa/main.rs".to_owned(), b"fn main() {}".to_vec());
    f.previews.insert(
        "mem:///casa/main.rs".to_owned(),
        norte_proto::methods::PluginPreviewStyled {
            plugin_id: "acme.syntax".to_owned(),
            plugin_name: "Syntax".to_owned(),
            lines: vec![
                vec![
                    norte_proto::methods::SpanWire {
                        text: "fn".to_owned(),
                        role: Some("title".to_owned()),
                        fg: Some([255, 0, 0]),
                        bg: None,
                    },
                    norte_proto::methods::SpanWire {
                        text: " main".to_owned(),
                        role: None,
                        fg: Some([0, 128, 255]),
                        bg: Some([0, 0, 64]),
                    },
                    norte_proto::methods::SpanWire {
                        // A role the theme does not know degrades to plain,
                        // and a bidi override from the plugin arrives
                        // masked.
                        text: "()\u{202e}{}".to_owned(),
                        role: Some("no-es-un-rol".to_owned()),
                        fg: None,
                        bg: None,
                    },
                ],
                vec![norte_proto::methods::SpanWire {
                    text: "plano".to_owned(),
                    role: None,
                    fg: None,
                    bg: None,
                }],
            ],
            lossy: false,
        },
    );
    let f = Arc::new(f);
    let (h, _snap) = host_tree(Arc::clone(&f)).await;
    let mut sub = h.subscribe();

    h.dispatch(press("F3")).await.expect("host alive");
    let mut viewer = None;
    for _ in 0..40 {
        h.dispatch(UiAction::Resync).await.expect("host alive");
        if let Some(v) = next_snapshot(&mut sub).await.viewer.clone()
            && !v.styled.is_empty()
        {
            viewer = Some(v);
            break;
        }
    }
    let v = viewer.expect("the viewer opens with the plugin's view");

    // The viewer's width travels with the request (0.66.0): it is the
    // viewport the host started with, not a `None` that lets the guest
    // choose.
    assert_eq!(
        f.anchos_de_preview.lock().expect("mutex").as_slice(),
        &[Some(120)],
        "one request, with the viewport's width"
    );

    assert_eq!(v.styled.len(), 2, "one entry per row: {:?}", v.styled);
    assert_eq!(v.styled.len(), v.lines.len(), "the same rows as `lines`");
    let first = &v.styled[0];
    assert_eq!(first.len(), 3);
    assert_eq!(first[0].text, "fn");
    assert_eq!(first[0].role.as_deref(), Some("title"));
    assert_eq!(first[0].fg.as_deref(), Some("#ff0000"));
    assert_eq!(first[1].role, None);
    assert_eq!(first[1].fg.as_deref(), Some("#0080ff"));
    assert_eq!(
        first[1].bg.as_deref(),
        Some("#000040"),
        "the background crosses over (bridge 50)"
    );
    assert_eq!(first[0].bg, None);
    assert_eq!(first[2].role, None, "an unknown role degrades to plain");
    assert!(
        !first[2].text.contains('\u{202e}'),
        "the bidi override does not cross raw: {:?}",
        first[2].text
    );
    assert_eq!(v.styled[1][0].text, "plano");
    assert_eq!(
        v.lines[0],
        first.iter().map(|s| s.text.as_str()).collect::<String>(),
        "`lines` is the same text, flattened"
    );
}

/// What the renderer MEASURED of the viewer's body (bridge 53) rules over
/// the viewport the next time it is opened: the viewport counts the chrome,
/// and an image shrunk to it used to stick out on the right.
#[tokio::test]
async fn the_viewer_requests_the_width_the_renderer_measured() {
    let mut f = Fake::default();
    f.put("mem:///casa", vec![(b"main.rs".to_vec(), false)]);
    f.content
        .insert("mem:///casa/main.rs".to_owned(), b"fn main() {}".to_vec());
    let f = Arc::new(f);
    let (h, _snap) = host_tree(Arc::clone(&f)).await;
    let mut sub = h.subscribe();
    h.dispatch(press("F3")).await.expect("host alive");
    assert_eq!(
        preview_widths_after(&h, &mut sub, &f, 1).await.as_slice(),
        &[Some(120)]
    );

    h.dispatch(UiAction::SetViewerCols { cols: 77 })
        .await
        .expect("host alive");
    h.dispatch(press("Escape")).await.expect("host alive");
    h.dispatch(press("F3")).await.expect("host alive");
    assert_eq!(
        preview_widths_after(&h, &mut sub, &f, 2).await.as_slice(),
        &[Some(120), Some(77)],
        "the second opening requests the measured width"
    );
}

/// Requests snapshots until the fake backend has seen `n` styled-preview
/// requests, and returns the widths they carried.
pub(super) async fn preview_widths_after(
    h: &UiHost,
    sub: &mut norte_ui_host::UiSubscription,
    f: &Fake,
    n: usize,
) -> Vec<Option<u32>> {
    let mut widths = Vec::new();
    for _ in 0..20 {
        h.dispatch(UiAction::Resync).await.expect("host alive");
        let _ = next_snapshot(sub).await;
        widths.clone_from(&f.anchos_de_preview.lock().expect("mutex"));
        if widths.len() >= n {
            break;
        }
    }
    widths
}

/// A previewer that does not apply does NOT get in the way: the viewer shows
/// the file.
#[tokio::test]
async fn with_no_previewer_the_viewer_shows_the_file() {
    let mut f = Fake::default();
    f.put("mem:///casa", vec![(b"notas.txt".to_vec(), false)]);
    f.content.insert(
        "mem:///casa/notas.txt".to_owned(),
        b"hola\nmundo\n".to_vec(),
    );
    let (h, _snap) = host_tree(Arc::new(f)).await;
    let mut sub = h.subscribe();

    h.dispatch(press("F3")).await.expect("host alive");
    for _ in 0..20 {
        h.dispatch(UiAction::Resync).await.expect("host alive");
        if let Some(v) = next_snapshot(&mut sub).await.viewer.clone() {
            assert!(v.lines.iter().any(|l| l.contains("hola")));
            assert!(
                v.preview_by.is_empty(),
                "with no plugin nothing gets attributed to anyone: {:?}",
                v.preview_by
            );
            return;
        }
    }
    panic!("the viewer opens the same with no previewer");
}

/// A previewer's name is THIRD-PARTY text and arrives masked.
#[tokio::test]
async fn a_previewers_name_arrives_masked() {
    let mut f = Fake::default();
    f.put("mem:///casa", vec![(b"x.bin".to_vec(), false)]);
    f.content
        .insert("mem:///casa/x.bin".to_owned(), b"\x00\x01".to_vec());
    f.previews.insert(
        "mem:///casa/x.bin".to_owned(),
        preview_de("ACME\u{202e}gpj", &["contenido"], false),
    );
    let (h, _snap) = host_tree(Arc::new(f)).await;
    let mut sub = h.subscribe();

    h.dispatch(press("F3")).await.expect("host alive");
    for _ in 0..20 {
        h.dispatch(UiAction::Resync).await.expect("host alive");
        if let Some(v) = next_snapshot(&mut sub).await.viewer.clone() {
            assert!(
                !v.preview_by.contains('\u{202e}'),
                "the plugin's name goes through raw: {:?}",
                v.preview_by
            );
            return;
        }
    }
    panic!("the viewer opens");
}

// ---------------------------------------------------------------------------
// The viewer's image (task 4.3, ADR 0069).
// ---------------------------------------------------------------------------

/// A PNG whose HEADER declares `w`x`h`, padded to `bytes`.
pub(super) fn png_de(w: u32, h: u32, bytes: usize) -> Vec<u8> {
    let mut v = b"\x89PNG\r\n\x1a\n".to_vec();
    v.extend_from_slice(&[0, 0, 0, 13]);
    v.extend_from_slice(b"IHDR");
    v.extend_from_slice(&w.to_be_bytes());
    v.extend_from_slice(&h.to_be_bytes());
    v.resize(bytes.max(v.len()), 0);
    v
}

/// The viewer opens the image: it says its format and its DECLARED size, and
/// its bytes do NOT travel in the snapshot.
#[tokio::test]
async fn an_image_is_accepted_and_its_bytes_travel_separately() {
    let mut f = Fake::default();
    f.put("mem:///casa", vec![(b"foto.png".to_vec(), false)]);
    f.content
        .insert("mem:///casa/foto.png".to_owned(), png_de(1920, 1080, 4096));
    let (h, _snap) = host_tree(Arc::new(f)).await;
    let mut sub = h.subscribe();

    h.dispatch(press("F3")).await.expect("host alive");
    let mut v = None;
    for _ in 0..20 {
        h.dispatch(UiAction::Resync).await.expect("host alive");
        if let Some(x) = next_snapshot(&mut sub).await.viewer.clone() {
            v = Some(x);
            break;
        }
    }
    let v = v.expect("the viewer opens");
    let img = v.image.clone().expect("the image is recognized");
    assert_eq!(img.format, "PNG", "by MAGIC bytes, not by extension");
    assert_eq!((img.width, img.height), (1920, 1080));
    assert!(v.image_refused.is_empty());

    // The bytes are NOT in the snapshot: eight megs in the patch stream is a
    // message that gets resent whole on every `Resync`.
    let snap = serde_json::to_string(&v).expect("serializes");
    assert!(
        snap.len() < 4096,
        "the viewer's view weighs {} bytes: the image's have slipped in",
        snap.len()
    );
    // And they are served through their own path.
    let bytes = h
        .image_bytes()
        .await
        .expect("host alive")
        .expect("there are bytes");
    assert_eq!(bytes.len(), 4096);
}

/// A header declaring a BOMB is rejected, and it says so.
///
/// A four-kilobyte PNG can declare 60000×60000 — 36 gigapixels — and cost
/// the decoder gigabytes. The header is read and refused BEFORE anyone
/// decodes, which is the only cheap defense (ADR 0069).
#[tokio::test]
async fn a_header_declaring_a_bomb_is_rejected() {
    let mut f = Fake::default();
    f.put("mem:///casa", vec![(b"bomba.png".to_vec(), false)]);
    f.content.insert(
        "mem:///casa/bomba.png".to_owned(),
        png_de(60000, 60000, 4096),
    );
    let (h, _snap) = host_tree(Arc::new(f)).await;
    let mut sub = h.subscribe();

    h.dispatch(press("F3")).await.expect("host alive");
    for _ in 0..20 {
        h.dispatch(UiAction::Resync).await.expect("host alive");
        let Some(v) = next_snapshot(&mut sub).await.viewer.clone() else {
            continue;
        };
        assert!(v.image.is_none(), "it is not painted");
        assert!(
            !v.image_refused.is_empty(),
            "and it SAYS so: silently falling back to the hex view looks \
             like norte is broken, not like norte being careful"
        );
        assert!(
            !v.image_refused.starts_with("viewer-image"),
            "translated, not the key: {:?}",
            v.image_refused
        );
        assert!(
            h.image_bytes().await.expect("host alive").is_none(),
            "and its bytes are served to nobody"
        );
        return;
    }
    panic!("the viewer opens the same");
}

/// A header that is not understood is also rejected.
///
/// "I don't know" treated as "go ahead" is the door this budget exists to
/// close.
#[tokio::test]
async fn a_header_that_is_not_understood_is_rejected() {
    let mut f = Fake::default();
    f.put("mem:///casa", vec![(b"raro.png".to_vec(), false)]);
    // Valid PNG signature, but the first chunk is NOT IHDR.
    let mut broken = b"\x89PNG\r\n\x1a\n".to_vec();
    broken.extend_from_slice(&[0, 0, 0, 13]);
    broken.extend_from_slice(b"iTXt");
    broken.resize(64, 0);
    f.content.insert("mem:///casa/raro.png".to_owned(), broken);
    let (h, _snap) = host_tree(Arc::new(f)).await;
    let mut sub = h.subscribe();

    h.dispatch(press("F3")).await.expect("host alive");
    for _ in 0..20 {
        h.dispatch(UiAction::Resync).await.expect("host alive");
        let Some(v) = next_snapshot(&mut sub).await.viewer.clone() else {
            continue;
        };
        assert!(v.image.is_none());
        assert!(!v.image_refused.is_empty(), "it says it is not understood");
        return;
    }
    panic!("the viewer opens the same");
}

/// Closing the viewer RELEASES the bytes: they are megabytes.
#[tokio::test]
async fn closing_the_viewer_releases_the_image() {
    let mut f = Fake::default();
    f.put("mem:///casa", vec![(b"foto.png".to_vec(), false)]);
    f.content
        .insert("mem:///casa/foto.png".to_owned(), png_de(64, 64, 2048));
    let (h, _snap) = host_tree(Arc::new(f)).await;
    let mut sub = h.subscribe();

    h.dispatch(press("F3")).await.expect("host alive");
    for _ in 0..20 {
        h.dispatch(UiAction::Resync).await.expect("host alive");
        if next_snapshot(&mut sub).await.viewer.is_some() {
            break;
        }
    }
    assert!(h.image_bytes().await.expect("host alive").is_some());

    h.dispatch(press("Escape")).await.expect("host alive");
    assert!(
        h.image_bytes().await.expect("host alive").is_none(),
        "a closed viewer holds onto no image megabytes"
    );
}

/// A file the viewer does not know how to paint as an image, and a thumbnail
/// plugin that does (ADR 0107): the viewer advertises the thumbnail as an
/// image, says whose it is, serves its bytes through the same channel as a
/// native image, and releases them on close.
#[tokio::test]
async fn the_viewer_shows_a_plugins_thumbnail_and_says_whose_it_is() {
    let mut f = Fake::default();
    f.put("mem:///casa", vec![(b"informe.pdf".to_vec(), false)]);
    f.content.insert(
        "mem:///casa/informe.pdf".to_owned(),
        b"%PDF-1.7 crudo".to_vec(),
    );
    f.thumbnails.insert(
        "mem:///casa/informe.pdf".to_owned(),
        norte_proto::methods::PluginThumbnail {
            plugin_id: "org.acme.thumbs".to_owned(),
            plugin_name: "Miniaturas ACME".to_owned(),
            mimetype: "image/png".to_owned(),
            bytes: b"\x89PNG\r\n\x1a\nno hace falta que sea real aqui".to_vec(),
            width: 120,
            height: 60,
        },
    );
    let (h, _snap) = host_tree(Arc::new(f)).await;
    let mut sub = h.subscribe();

    h.dispatch(press("F3")).await.expect("host alive");
    let mut viewer = None;
    for _ in 0..40 {
        h.dispatch(UiAction::Resync).await.expect("host alive");
        if let Some(v) = next_snapshot(&mut sub).await.viewer.clone()
            && v.image.is_some()
        {
            viewer = Some(v);
            break;
        }
    }
    let v = viewer.expect("the viewer opens with the thumbnail");
    let img = v.image.expect("it advertises an image");
    assert_eq!(
        (img.format.as_str(), img.width, img.height),
        ("PNG", 120, 60)
    );
    assert!(
        v.preview_by.contains("Miniaturas ACME"),
        "and says whose it is: {:?}",
        v.preview_by
    );
    assert_eq!(
        v.image_refused, "",
        "with a thumbnail there is no reason to give"
    );
    let bytes = h
        .image_bytes()
        .await
        .expect("host alive")
        .expect("the bytes");
    assert!(bytes.starts_with(b"\x89PNG"), "the thumbnail's");

    h.dispatch(press("Escape")).await.expect("host alive");
    assert!(h.image_bytes().await.expect("host alive").is_none());
}
