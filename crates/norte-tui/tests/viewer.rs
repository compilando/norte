//! Tests for the TUI viewer's status (phase 7 / GUI-d T2): the i18n
//! composition (`norte_tui::viewer::status`) over the SHARED core Viewer
//! (`norte_frontend::viewer::Viewer`, re-exported by `norte_tui::viewer`).
//! The purely core tests (decoding, hex, scroll with no status) live in
//! `norte-frontend` (GUI-d T1) — they are not duplicated here.

use norte_proto::VPath;
use norte_tui::viewer::{Viewer, status};

fn vp() -> VPath {
    // The status asserts are in Spanish: fixes the process's language
    // (nextest = one process per test; first call wins).
    let _ = norte_i18n::force(norte_i18n::Lang::Es);
    VPath::parse("file:///f").unwrap()
}

#[test]
fn texto_del_corpus_se_ve_decodificado() {
    for f in norte_testkit::corpus::content_fixtures() {
        let v = Viewer::new(vp(), f.bytes.clone(), false);
        assert!(!v.hex, "{}: text, not hexview", f.id);
        let rows = v.rows(10);
        // The viewer NEUTRALIZES controls to `�` on paint (`render_line`):
        // the corpus includes a fixture with raw controls
        // (`preview_bidi_ctrl_injection`), so the expected value is the
        // first decoded line with that same neutralization (clean fixtures
        // have no controls → expected identical to decoded).
        let expected: Option<String> = f.decoded.lines().next().map(|l| {
            l.chars()
                .map(|c| {
                    if norte_encoding::is_terminal_hazard(c) {
                        '\u{FFFD}'
                    } else {
                        c
                    }
                })
                .collect()
        });
        assert_eq!(
            rows.first().cloned(),
            expected,
            "{}: first decoded line (controls neutralized)",
            f.id
        );
        assert!(
            !status(&v).contains("pérdidas"),
            "{}: no losses with detection",
            f.id
        );
    }
}

#[test]
fn recargar_como_cicla_y_marca_forzado() {
    // latin1: detection gives windows-1252; forcing UTF-8 produces losses.
    let bytes = b"a\xF1o 2026\n".to_vec();
    let mut v = Viewer::new(vp(), bytes, false);
    assert!(status(&v).contains("windows-1252"), "{}", status(&v));
    assert!(!status(&v).contains("forzado"));
    v.cycle_encoding(); // "reload as…" → forced encoding
    assert!(status(&v).contains("UTF-8") && status(&v).contains("forzado"));
    assert!(
        status(&v).contains("pérdidas"),
        "0xF1 is not valid UTF-8: a VISIBLE loss — {}",
        status(&v)
    );
    v.reset_encoding();
    assert!(status(&v).contains("windows-1252") && !status(&v).contains("forzado"));
}

#[test]
fn scroll_con_topes_y_truncado_visible() {
    use std::fmt::Write;
    let mut text = String::new();
    for i in 0..50 {
        let _ = writeln!(text, "línea {i}");
    }
    let mut v = Viewer::new(vp(), text.into_bytes(), true);
    assert!(status(&v).contains("[cabecera]"), "{}", status(&v));
    assert!(
        status(&v).contains("LF"),
        "EOL in the status: {}",
        status(&v)
    );
    v.scroll_up(5);
    assert_eq!(v.scroll, 0);
    v.scroll_down(10);
    assert_eq!(v.scroll, 10);
    v.scroll_bottom();
    assert_eq!(v.scroll, 49);
    v.scroll_down(5);
    assert_eq!(v.scroll, 49, "bottom cap");
    v.scroll_top();
    assert_eq!(v.scroll, 0);
    assert_eq!(v.rows(3).len(), 3);
}

/// H6: CR-only (classic Mac) splits lines to PAINT; the real EOL is still
/// announced in the status.
#[test]
fn cr_only_se_parte_en_lineas() {
    let v = Viewer::new(vp(), b"uno\rdos\rtres\r".to_vec(), false);
    assert_eq!(v.total_rows(), 3);
    assert_eq!(v.rows(3), vec!["uno", "dos", "tres"]);
    assert!(status(&v).contains("CR"), "{}", status(&v));
}

/// **The LEFT-hand clip keeps the grid, and no row ever starts on
/// something zero-width.**
///
/// Swept over `viewer_grid_lines`, the fixture made for this: six lines
/// over two hundred columns long, each hostile to a clip for a different
/// reason — two-cell ideograms, NFD, a ZWJ cluster, VS16 and pairs of
/// regional indicators, and a tab behind a wide char. Both properties are
/// checked at EVERY possible offset instead of pinning one magic value,
/// the way the tail's twin
/// (`middle_ellipsis_jamas_deja_la_cola_empezando_en_ancho_cero`) is
/// written.
///
/// The first property is what makes a CSV or an aligned log readable: if
/// one row loses a column another does not, the two stop lining up and
/// horizontal scroll stops doing the one thing it is for.
#[test]
fn el_corte_por_la_izquierda_mantiene_la_rejilla() {
    use unicode_width::UnicodeWidthChar;

    let lineas = norte_testkit::corpus::viewer_grid_lines();
    let texto: String = lineas
        .iter()
        .map(|l| format!("{}\n", l.text))
        .collect::<Vec<_>>()
        .concat();
    let mut v = Viewer::new(vp(), texto.into_bytes(), false);
    let alto = lineas.len();
    let anchas: Vec<usize> = v
        .rows(alto)
        .iter()
        .map(|f| norte_frontend::cells(f))
        .collect();
    let tope = v.max_cols();
    assert!(tope >= 200, "the fixture is wide on purpose: {tope}");

    for pedido in 0..=tope {
        v.scroll_left(usize::MAX);
        v.scroll_right(pedido);
        // The REAL scroll, not the requested one: the cap always leaves one
        // column visible, so the last round gets bounded.
        let h = v.hscroll();
        let filas = v.rows(alto);
        for (i, fila) in filas.iter().enumerate() {
            let id = lineas[i].id;
            let visto = norte_frontend::cells(fila);
            assert_eq!(
                visto + h.min(anchas[i]),
                anchas[i],
                "`{id}` scrolled {h}: loses or gains columns relative to the \
                 others, so its columns stop lining up ({})",
                lineas[i].why
            );
            if let Some(c) = fila.chars().next() {
                assert_ne!(
                    UnicodeWidthChar::width(c),
                    Some(0),
                    "`{id}` scrolled {h} starts at zero width: the mark lost \
                     its base on the other side of the clip and reparents to \
                     the next letter ({})",
                    lineas[i].why
                );
            }
        }
    }
}

/// And hex scrolls the SAME way, with its own width: its rows are 77 cells,
/// and in a split slot the right-hand ASCII column does not fit.
#[test]
fn el_hexadecimal_del_corpus_tambien_se_desplaza() {
    let f = norte_testkit::corpus::content_fixtures()
        .into_iter()
        .find(|f| f.id == "utf16le_bom")
        .expect("a BOM fixture that reads as text");
    let mut v = Viewer::new(vp(), f.bytes.clone(), false);
    v.toggle_hex();
    assert!(v.hex);
    let entera = v.rows(1)[0].clone();
    assert_eq!(v.max_cols(), 77, "the DUMP's width, not the text's");
    v.scroll_right(11);
    assert_eq!(v.rows(1)[0], entera[11..]);
    assert!(
        status(&v).contains("12/77"),
        "and the column IS stated, which is the only thing that states it \
         in the docked viewer: {}",
        status(&v)
    );
}

/// H1 applied to the viewer: forcing windows-1252 over a spurious BOM shows
/// it as DATA (þÿ), not as UTF-16.
#[test]
fn recargar_como_vence_al_bom() {
    let f = norte_testkit::corpus::content_fixtures_forced()
        .into_iter()
        .find(|f| f.id == "w1252_fake_bom")
        .unwrap();
    let mut v = Viewer::new(vp(), f.bytes.clone(), false);
    // Cycle through to windows-1252.
    for _ in 0..norte_encoding::reload_cycle().len() {
        v.cycle_encoding();
        if status(&v).starts_with("windows-1252") {
            break;
        }
    }
    assert!(status(&v).starts_with("windows-1252"), "{}", status(&v));
    assert_eq!(v.rows(1)[0], "þÿ Fahr.");
    assert!(!status(&v).contains("pérdidas"), "{}", status(&v));
}
