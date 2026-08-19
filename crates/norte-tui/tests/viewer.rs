//! Tests de la status del viewer de la TUI (fase 7 / GUI-d T2): la
//! composición i18n (`norte_tui::viewer::status`) sobre el Viewer core
//! COMPARTIDO (`norte_frontend::viewer::Viewer`, re-exportado por
//! `norte_tui::viewer`). Los tests puramente del core (decodificación, hex,
//! scroll sin status) viven en `norte-frontend` (GUI-d T1) — no se duplican
//! aquí.

use norte_proto::VPath;
use norte_tui::viewer::{Viewer, status};

fn vp() -> VPath {
    // Los asserts de status son en español: fija el idioma del proceso
    // (nextest = un proceso por test; primera llamada gana).
    let _ = norte_i18n::force(norte_i18n::Lang::Es);
    VPath::parse("file:///f").unwrap()
}

#[test]
fn texto_del_corpus_se_ve_decodificado() {
    for f in norte_testkit::corpus::content_fixtures() {
        let v = Viewer::new(vp(), f.bytes.clone(), false);
        assert!(!v.hex, "{}: texto, no hexview", f.id);
        let rows = v.rows(10);
        // El viewer NEUTRALIZA los controles a `�` al pintar (`render_line`):
        // el corpus incluye una fixture con controles crudos
        // (`preview_bidi_ctrl_injection`), así que el esperado es la primera
        // línea decodificada con esa misma neutralización (los fixtures limpios
        // no tienen controles → esperado idéntico al decoded).
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
            "{}: primera línea decodificada (controles neutralizados)",
            f.id
        );
        assert!(
            !status(&v).contains("pérdidas"),
            "{}: sin pérdidas con la detección",
            f.id
        );
    }
}

#[test]
fn recargar_como_cicla_y_marca_forzado() {
    // latin1: la detección da windows-1252; forzar UTF-8 produce pérdidas.
    let bytes = b"a\xF1o 2026\n".to_vec();
    let mut v = Viewer::new(vp(), bytes, false);
    assert!(status(&v).contains("windows-1252"), "{}", status(&v));
    assert!(!status(&v).contains("forzado"));
    v.cycle_encoding(); // «recargar como…» → encoding forzado
    assert!(status(&v).contains("UTF-8") && status(&v).contains("forzado"));
    assert!(
        status(&v).contains("pérdidas"),
        "0xF1 no es UTF-8 válido: pérdida VISIBLE — {}",
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
        "EOL en la status: {}",
        status(&v)
    );
    v.scroll_up(5);
    assert_eq!(v.scroll, 0);
    v.scroll_down(10);
    assert_eq!(v.scroll, 10);
    v.scroll_bottom();
    assert_eq!(v.scroll, 49);
    v.scroll_down(5);
    assert_eq!(v.scroll, 49, "tope inferior");
    v.scroll_top();
    assert_eq!(v.scroll, 0);
    assert_eq!(v.rows(3).len(), 3);
}

/// H6: CR-only (Mac clásico) parte líneas para PINTAR; el EOL real se sigue
/// anunciando en la status.
#[test]
fn cr_only_se_parte_en_lineas() {
    let v = Viewer::new(vp(), b"uno\rdos\rtres\r".to_vec(), false);
    assert_eq!(v.total_rows(), 3);
    assert_eq!(v.rows(3), vec!["uno", "dos", "tres"]);
    assert!(status(&v).contains("CR"), "{}", status(&v));
}

/// H1 aplicado al viewer: forzar windows-1252 sobre un BOM espurio lo
/// muestra como DATO (þÿ), no como UTF-16.
#[test]
fn recargar_como_vence_al_bom() {
    let f = norte_testkit::corpus::content_fixtures_forced()
        .into_iter()
        .find(|f| f.id == "w1252_fake_bom")
        .unwrap();
    let mut v = Viewer::new(vp(), f.bytes.clone(), false);
    // Ciclar hasta windows-1252.
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
