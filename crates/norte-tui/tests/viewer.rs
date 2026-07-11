//! Tests del estado del viewer (fase 7): detección, «recargar como…»,
//! hexview y scroll — sobre el corpus de contenidos del testkit.

use norte_proto::VPath;
use norte_tui::viewer::Viewer;

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
        assert_eq!(
            rows.first().map(String::as_str),
            f.decoded.lines().next(),
            "{}: primera línea decodificada",
            f.id
        );
        assert!(
            !v.status().contains("pérdidas"),
            "{}: sin pérdidas con la detección",
            f.id
        );
    }
}

#[test]
fn binario_cae_a_hexview_y_el_toggle_vuelve() {
    let png = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR".to_vec();
    let mut v = Viewer::new(vp(), png, false);
    assert!(v.hex, "NUL sin BOM = hexview automático (spec §6)");
    let rows = v.rows(4);
    assert!(rows[0].starts_with("00000000"), "offset: {}", rows[0]);
    assert!(rows[0].contains("89 50 4e 47"), "hex: {}", rows[0]);
    assert!(rows[0].contains("PNG"), "gutter ascii: {}", rows[0]);
    // Toggle manual: sale del hex (texto vacío en binario, pero es SU
    // decisión); x de nuevo vuelve.
    v.toggle_hex();
    assert!(!v.hex);
    v.toggle_hex();
    assert!(v.hex);
}

#[test]
fn recargar_como_cicla_y_marca_forzado() {
    // latin1: la detección da windows-1252; forzar UTF-8 produce pérdidas.
    let bytes = b"a\xF1o 2026\n".to_vec();
    let mut v = Viewer::new(vp(), bytes, false);
    assert!(v.status().contains("windows-1252"), "{}", v.status());
    assert!(!v.status().contains("forzado"));
    v.cycle_encoding(); // → UTF-8 (primero del ciclo)
    assert!(v.status().contains("UTF-8") && v.status().contains("forzado"));
    assert!(
        v.status().contains("pérdidas"),
        "0xF1 no es UTF-8 válido: pérdida VISIBLE — {}",
        v.status()
    );
    v.reset_encoding();
    assert!(v.status().contains("windows-1252") && !v.status().contains("forzado"));
}

#[test]
fn scroll_con_topes_y_truncado_visible() {
    use std::fmt::Write;
    let mut texto = String::new();
    for i in 0..50 {
        let _ = writeln!(texto, "línea {i}");
    }
    let mut v = Viewer::new(vp(), texto.into_bytes(), true);
    assert!(v.status().contains("[cabecera]"), "{}", v.status());
    assert!(
        v.status().contains("LF"),
        "EOL en la status: {}",
        v.status()
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

/// H4/H5 de la auditoría: tabs EXPANDIDOS (ratatui los borraría) y ESC
/// enmascarado — jamás alteración sin marca.
#[test]
fn tabs_expandidos_y_controles_enmascarados() {
    let v = Viewer::new(vp(), b"all:\n\tcc -o x x.c\n".to_vec(), false);
    let rows = v.rows(3);
    assert_eq!(rows[0], "all:");
    assert_eq!(rows[1], "        cc -o x x.c", "tab → 8 espacios");
    let v = Viewer::new(vp(), b"rojo:\x1b[31mX\n".to_vec(), false);
    assert_eq!(
        v.rows(2)[0],
        "rojo:\u{FFFD}[31mX",
        "ESC visible como \u{FFFD}"
    );
}

/// H6: CR-only (Mac clásico) parte líneas para PINTAR; el EOL real se
/// sigue anunciando en la status.
#[test]
fn cr_only_se_parte_en_lineas() {
    let v = Viewer::new(vp(), b"uno\rdos\rtres\r".to_vec(), false);
    assert_eq!(v.total_rows(), 3);
    assert_eq!(v.rows(3), vec!["uno", "dos", "tres"]);
    assert!(v.status().contains("CR"), "{}", v.status());
}

/// H7: togglear a hex con scroll alto reclampa (jamás pantalla en blanco).
#[test]
fn toggle_hex_reclampa_el_scroll() {
    use std::fmt::Write;
    let mut texto = String::new();
    for i in 0..100 {
        let _ = writeln!(texto, "{i}");
    }
    let mut v = Viewer::new(vp(), texto.into_bytes(), false);
    v.scroll_bottom();
    assert_eq!(v.scroll, 99);
    v.toggle_hex();
    assert!(v.scroll < v.total_rows(), "reclampado: {}", v.scroll);
    assert!(!v.rows(5).is_empty(), "el hexview pinta algo");
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
        if v.status().starts_with("windows-1252") {
            break;
        }
    }
    assert!(v.status().starts_with("windows-1252"), "{}", v.status());
    assert_eq!(v.rows(1)[0], "þÿ Fahr.");
    assert!(!v.status().contains("pérdidas"), "{}", v.status());
}
