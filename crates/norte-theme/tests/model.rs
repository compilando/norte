//! Modelo de theming: parseo de color, degradación de profundidad y
//! resolución de roles (ADR 0020, fase T2).

use norte_theme::{Color, ColorDepth, FileKind, ResolvedColor, Role, Theme, extension_of};
use proptest::prelude::*;

#[test]
fn color_parse_formas() {
    assert_eq!(
        Color::parse("#ff8800").unwrap(),
        Color::rgb(0xff, 0x88, 0x00)
    );
    // `#rgb` expande cada dígito.
    assert_eq!(Color::parse("#f80").unwrap(), Color::rgb(0xff, 0x88, 0x00));
    assert!(Color::parse("ff8800").is_err()); // sin `#`
    assert!(Color::parse("#ff88").is_err()); // longitud rara
    assert!(Color::parse("#gg0000").is_err()); // dígito no hex
}

#[test]
fn truecolor_es_identidad() {
    let c = Color::rgb(0x12, 0x34, 0x56);
    assert_eq!(
        c.resolve(ColorDepth::Truecolor),
        ResolvedColor::Rgb(0x12, 0x34, 0x56)
    );
}

#[test]
fn degradacion_en_rango() {
    for c in [
        Color::rgb(0, 0, 0),
        Color::rgb(255, 255, 255),
        Color::rgb(137, 180, 250),
    ] {
        let ResolvedColor::Indexed(low) = c.resolve(ColorDepth::Ansi16) else {
            panic!("ansi16 debe ser Indexed");
        };
        assert!(low <= 15, "índice ANSI-16 fuera de rango: {low}");
        let ResolvedColor::Indexed(mid) = c.resolve(ColorDepth::Ansi256) else {
            panic!("ansi256 debe ser Indexed");
        };
        // El cubo/grises viven en 16..=255.
        assert!(
            (16..=255).contains(&mid),
            "índice 256 fuera de rango: {mid}"
        );
    }
}

#[test]
fn negro_y_blanco_degradan_a_extremos_ansi16() {
    // Negro → 0; blanco → 15 (extremos exactos de la tabla ANSI-16).
    assert_eq!(
        Color::rgb(0, 0, 0).resolve(ColorDepth::Ansi16),
        ResolvedColor::Indexed(0)
    );
    assert_eq!(
        Color::rgb(255, 255, 255).resolve(ColorDepth::Ansi16),
        ResolvedColor::Indexed(15)
    );
}

#[test]
fn tema_parcial_hereda_fallback() {
    let t = Theme::from_toml(
        r##"
        name = "parcial"
        [roles]
        selection = { bg = "#45475a" }
    "##,
    )
    .unwrap();
    // Rol definido: reemplaza el fallback (bg, SIN el reverse del fallback).
    let sel = t.style(Role::Selection);
    assert_eq!(sel.bg, Some(Color::rgb(0x45, 0x47, 0x5a)));
    assert!(
        !sel.reverse,
        "un rol explícito reemplaza el fallback entero"
    );
    // Rol ausente: fallback monocromo de M1 (borde con foco = negrita).
    assert!(t.style(Role::BorderFocus).bold);
}

#[test]
fn efectos_opacos_no_rompen_el_parseo() {
    // La TUI ignora [effects]; el parser lo acepta como opaco (ADR 0020 D4).
    let t = Theme::from_toml(
        r##"
        [effects.glow]
        radius = 4
        color = "#cba6f7"
    "##,
    )
    .unwrap();
    assert!(t.has_effects());
    // Y los roles siguen resolviendo por fallback.
    assert!(t.style(Role::BorderFocus).bold);
}

/// C2/G0: the three GUI-chrome roles exist, are in ALL, and have a
/// usable monochrome fallback.
#[test]
fn roles_de_chrome_gui_presentes() {
    for r in [Role::PaneBackground, Role::PaneFocusBackground, Role::Mark] {
        assert!(Role::ALL.contains(&r));
        let _ = r.fallback();
    }
}

#[test]
fn extension_de_nombres() {
    assert_eq!(extension_of(b"foto.PNG"), Some(&b"PNG"[..]));
    assert_eq!(extension_of(b"a.tar.gz"), Some(&b"gz"[..])); // último punto
    assert_eq!(extension_of(b".bashrc"), None); // oculto sin extensión
    assert_eq!(extension_of(b"README"), None); // sin punto
    assert_eq!(extension_of(b"trailing."), None); // punto final
    // Extensión con bytes no-UTF8: se devuelve cruda (regla 1).
    assert_eq!(extension_of(&[b'x', b'.', 0xFF]), Some(&[0xFF][..]));
}

#[test]
fn file_style_prioridad_ext_sobre_kind() {
    let t = Theme::from_toml(
        r##"
        [files.kind]
        dir = { fg = "#89b4fa" }
        executable = { fg = "#a6e3a1", bold = true }
        [files.ext]
        rs = { fg = "#f74c00" }
    "##,
    )
    .unwrap();
    // Extensión gana (case-insensitive) aunque el kind sea Regular.
    assert_eq!(
        t.file_style(b"main.RS", FileKind::Regular).fg,
        Some(Color::rgb(0xf7, 0x4c, 0x00))
    );
    // Sin extensión conocida: cae al kind.
    let dir = t.file_style(b"src", FileKind::Dir);
    assert_eq!(dir.fg, Some(Color::rgb(0x89, 0xb4, 0xfa)));
    // Sin ext ni kind coloreado: rol regular (aquí, fallback vacío).
    assert_eq!(t.file_style(b"LICENSE", FileKind::Regular).fg, None);
}

proptest! {
    /// `parse(to_hex(c)) == c` para cualquier color (roundtrip byte-exacto).
    #[test]
    fn color_roundtrip(r in any::<u8>(), g in any::<u8>(), b in any::<u8>()) {
        let c = Color::rgb(r, g, b);
        prop_assert_eq!(Color::parse(&c.to_hex()).unwrap(), c);
    }

    /// La degradación es TOTAL y en rango para cualquier color y profundidad;
    /// truecolor es siempre identidad.
    #[test]
    fn degradacion_total_y_en_rango(r in any::<u8>(), g in any::<u8>(), b in any::<u8>()) {
        let c = Color::rgb(r, g, b);
        prop_assert_eq!(c.resolve(ColorDepth::Truecolor), ResolvedColor::Rgb(r, g, b));
        match c.resolve(ColorDepth::Ansi16) {
            ResolvedColor::Indexed(i) => prop_assert!(i <= 15),
            ResolvedColor::Rgb(..) => prop_assert!(false, "ansi16 no indexado"),
        }
        match c.resolve(ColorDepth::Ansi256) {
            ResolvedColor::Indexed(i) => prop_assert!((16..=255).contains(&i)),
            ResolvedColor::Rgb(..) => prop_assert!(false, "ansi256 no indexado"),
        }
    }

    /// Un color de la propia paleta ANSI-16 degrada a SÍ mismo (punto fijo):
    /// el más cercano a un color exacto de la tabla es él mismo.
    #[test]
    fn ansi16_punto_fijo(idx in 0u8..16) {
        // Reconstruye el color de la tabla vía su índice conocido por hex.
        let table = [
            "#000000","#800000","#008000","#808000","#000080","#800080","#008080","#c0c0c0",
            "#808080","#ff0000","#00ff00","#ffff00","#0000ff","#ff00ff","#00ffff","#ffffff",
        ];
        let c = Color::parse(table[idx as usize]).unwrap();
        prop_assert_eq!(c.resolve(ColorDepth::Ansi16), ResolvedColor::Indexed(idx));
    }
}
