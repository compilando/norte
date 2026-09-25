//! Theming model: color parsing, depth degradation and
//! role resolution (ADR 0020, phase T2).

use norte_theme::{Color, ColorDepth, FileKind, ResolvedColor, Role, Theme, extension_of};
use proptest::prelude::*;

#[test]
fn color_parse_forms() {
    assert_eq!(
        Color::parse("#ff8800").unwrap(),
        Color::rgb(0xff, 0x88, 0x00)
    );
    // `#rgb` expands each digit.
    assert_eq!(Color::parse("#f80").unwrap(), Color::rgb(0xff, 0x88, 0x00));
    assert!(Color::parse("ff8800").is_err()); // no `#`
    assert!(Color::parse("#ff88").is_err()); // odd length
    assert!(Color::parse("#gg0000").is_err()); // non-hex digit
}

#[test]
fn truecolor_is_identity() {
    let c = Color::rgb(0x12, 0x34, 0x56);
    assert_eq!(
        c.resolve(ColorDepth::Truecolor),
        ResolvedColor::Rgb(0x12, 0x34, 0x56)
    );
}

#[test]
fn degradation_in_range() {
    for c in [
        Color::rgb(0, 0, 0),
        Color::rgb(255, 255, 255),
        Color::rgb(137, 180, 250),
    ] {
        let ResolvedColor::Indexed(low) = c.resolve(ColorDepth::Ansi16) else {
            panic!("ansi16 must be Indexed");
        };
        assert!(low <= 15, "ANSI-16 index out of range: {low}");
        let ResolvedColor::Indexed(mid) = c.resolve(ColorDepth::Ansi256) else {
            panic!("ansi256 must be Indexed");
        };
        // The cube/grays live in 16..=255.
        assert!((16..=255).contains(&mid), "256 index out of range: {mid}");
    }
}

#[test]
fn black_and_white_degrade_to_ansi16_extremes() {
    // Black → 0; white → 15 (exact extremes of the ANSI-16 table).
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
fn partial_theme_inherits_fallback() {
    let t = Theme::from_toml(
        r##"
        name = "partial"
        [roles]
        selection = { bg = "#45475a" }
    "##,
    )
    .unwrap();
    // Defined role: replaces the fallback (bg, WITHOUT the fallback's reverse).
    let sel = t.style(Role::Selection);
    assert_eq!(sel.bg, Some(Color::rgb(0x45, 0x47, 0x5a)));
    assert!(!sel.reverse, "an explicit role replaces the whole fallback");
    // Absent role: M1 monochrome fallback (focused border = bold).
    assert!(t.style(Role::BorderFocus).bold);
}

#[test]
fn opaque_effects_do_not_break_parsing() {
    // The TUI ignores [effects]; the parser accepts it as opaque (ADR 0020 D4).
    let t = Theme::from_toml(
        r##"
        [effects.glow]
        radius = 4
        color = "#cba6f7"
    "##,
    )
    .unwrap();
    assert!(t.has_effects());
    // And the roles still resolve via fallback.
    assert!(t.style(Role::BorderFocus).bold);
}

/// C2/G0: the three GUI-chrome roles exist, are in ALL, and have a
/// usable monochrome fallback.
#[test]
fn gui_chrome_roles_present() {
    for r in [Role::PaneBackground, Role::PaneFocusBackground, Role::Mark] {
        assert!(Role::ALL.contains(&r));
        let _ = r.fallback();
    }
}

#[test]
fn name_extension() {
    assert_eq!(extension_of(b"foto.PNG"), Some(&b"PNG"[..]));
    assert_eq!(extension_of(b"a.tar.gz"), Some(&b"gz"[..])); // last dot
    assert_eq!(extension_of(b".bashrc"), None); // hidden without extension
    assert_eq!(extension_of(b"README"), None); // no dot
    assert_eq!(extension_of(b"trailing."), None); // trailing dot
    // Extension with non-UTF8 bytes: returned raw (rule 1).
    assert_eq!(extension_of(&[b'x', b'.', 0xFF]), Some(&[0xFF][..]));
}

#[test]
fn file_style_ext_takes_priority_over_kind() {
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
    // Extension wins (case-insensitive) even if the kind is Regular.
    assert_eq!(
        t.file_style(b"main.RS", FileKind::Regular).fg,
        Some(Color::rgb(0xf7, 0x4c, 0x00))
    );
    // No known extension: falls back to kind.
    let dir = t.file_style(b"src", FileKind::Dir);
    assert_eq!(dir.fg, Some(Color::rgb(0x89, 0xb4, 0xfa)));
    // Neither ext nor colored kind: regular role (here, empty fallback).
    assert_eq!(t.file_style(b"LICENSE", FileKind::Regular).fg, None);
}

proptest! {
    /// `parse(to_hex(c)) == c` for any color (byte-exact roundtrip).
    #[test]
    fn color_roundtrip(r in any::<u8>(), g in any::<u8>(), b in any::<u8>()) {
        let c = Color::rgb(r, g, b);
        prop_assert_eq!(Color::parse(&c.to_hex()).unwrap(), c);
    }

    /// Degradation is TOTAL and in range for any color and depth;
    /// truecolor is always identity.
    #[test]
    fn degradation_total_and_in_range(r in any::<u8>(), g in any::<u8>(), b in any::<u8>()) {
        let c = Color::rgb(r, g, b);
        prop_assert_eq!(c.resolve(ColorDepth::Truecolor), ResolvedColor::Rgb(r, g, b));
        match c.resolve(ColorDepth::Ansi16) {
            ResolvedColor::Indexed(i) => prop_assert!(i <= 15),
            ResolvedColor::Rgb(..) => prop_assert!(false, "ansi16 not indexed"),
        }
        match c.resolve(ColorDepth::Ansi256) {
            ResolvedColor::Indexed(i) => prop_assert!((16..=255).contains(&i)),
            ResolvedColor::Rgb(..) => prop_assert!(false, "ansi256 not indexed"),
        }
    }

    /// A color from the ANSI-16 palette itself degrades to ITSELF (fixed point):
    /// the nearest to an exact table color is itself.
    #[test]
    fn ansi16_fixed_point(idx in 0u8..16) {
        // Rebuilds the table color via its hex-known index.
        let table = [
            "#000000","#800000","#008000","#808000","#000080","#800080","#008080","#c0c0c0",
            "#808080","#ff0000","#00ff00","#ffff00","#0000ff","#ff00ff","#00ffff","#ffffff",
        ];
        let c = Color::parse(table[idx as usize]).unwrap();
        prop_assert_eq!(c.resolve(ColorDepth::Ansi16), ResolvedColor::Indexed(idx));
    }
}
