//! Todos los presets embebidos parsean, cubren cada rol y cada tipo de nodo
//! con color EXPLÍCITO (no solo fallback), y traen un juego de extensiones
//! (ADR 0020 D3, fase T3).

use norte_theme::{FileKind, Role, Theme, preset_names, preset_source};

const KINDS: &[FileKind] = &[
    FileKind::Dir,
    FileKind::Symlink,
    FileKind::Executable,
    FileKind::Fifo,
    FileKind::Socket,
    FileKind::BlockDevice,
    FileKind::CharDevice,
];

#[test]
fn cada_preset_parsea_y_es_completo() {
    let names = preset_names();
    assert!(names.contains(&"default"));
    assert!(names.contains(&"catppuccin-mocha"));
    assert!(names.contains(&"gruvbox-dark"));
    assert!(names.contains(&"nord"));
    assert!(names.contains(&"vscode-dark"));
    assert!(names.contains(&"vscode-light"));

    for name in names {
        let src = preset_source(name).expect("fuente del preset");
        let theme = Theme::from_toml(src).unwrap_or_else(|e| panic!("[{name}] no parsea: {e}"));
        assert_eq!(theme.name.as_deref(), Some(name), "[{name}] name coincide");

        // Cada rol tiene color de frente EXPLÍCITO (un preset colorea todo, no
        // se queda en el fallback monocromo).
        //
        // CORE, no ALL: los diez roles de CROMO (spec 2026-09-11, F2) se
        // derivan en la hoja de estilos de la ventana de colores que el tema
        // ya tiene, así que exigírselos a cada preset serían ochenta valores
        // inventados — y un `hover` monocromo no es un hover prudente, es uno
        // invisible. Los presets `vscode-*` sí los definen, porque para ellos
        // el cromo es el asunto.
        for &role in Role::CORE {
            let st = theme.style(role);
            assert!(
                st.fg.is_some() || st.bg.is_some(),
                "[{name}] el rol {role:?} no tiene color"
            );
        }

        // Cada tipo de nodo tiene color por kind.
        for &kind in KINDS {
            let st = theme.file_style(b"x", kind);
            assert!(st.fg.is_some(), "[{name}] el kind {kind:?} no tiene color");
        }

        // Y las extensiones habituales colorean (ext gana al kind).
        for ext in ["main.rs", "a.zip", "foto.png", "readme.md"] {
            assert!(
                theme
                    .file_style(ext.as_bytes(), FileKind::Regular)
                    .fg
                    .is_some(),
                "[{name}] la extensión de {ext} no colorea"
            );
        }
    }
}

/// G1: los presets retro declaran [effects] (la GUI los interpreta; la TUI
/// los ignora) y pasan la misma completitud que el resto.
#[test]
fn presets_retro_traen_effects() {
    for name in ["retro-crt", "retro-crt-amber"] {
        let t = norte_theme::Theme::preset(name)
            .expect("parsea")
            .expect("preset registrado");
        assert!(t.has_effects(), "{name} sin [effects]");
    }
}

#[test]
fn preset_por_nombre_y_default() {
    assert_eq!(
        Theme::preset("nord").unwrap().unwrap().name.as_deref(),
        Some("nord")
    );
    // Nombre desconocido = None (el caller lo tratará como ruta).
    assert!(Theme::preset("no-existe").unwrap().is_none());
    // El default siempre está.
    assert_eq!(Theme::preset_default().name.as_deref(), Some("default"));
}

/// Las señales SEMÁNTICAS (`error`, `warning` y sobre todo `hostile-badge`,
/// que marca un nombre enmascarado — spec §6, superficie de seguridad) no
/// pueden quedarse por debajo del 4.5:1 de WCAG AA contra el fondo de SU
/// tema: son exactamente las que hay que leer cuando algo va mal. El resto
/// de la paleta (bordes, acentos, barra de estado) queda fuera a propósito
/// —ahí manda el gusto del tema— y su suelo, más bajo, lo pinea el render de
/// `norte-tui`.
#[test]
fn las_senales_semanticas_llegan_a_wcag_aa_en_todo_preset() {
    fn luminancia(c: norte_theme::Color) -> f64 {
        let canal = |v: u8| {
            let s = f64::from(v) / 255.0;
            if s <= 0.039_28 {
                s / 12.92
            } else {
                ((s + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * canal(c.r) + 0.7152 * canal(c.g) + 0.0722 * canal(c.b)
    }
    fn contraste(a: norte_theme::Color, b: norte_theme::Color) -> f64 {
        let (l1, l2) = (luminancia(a), luminancia(b));
        let (alto, bajo) = if l1 > l2 { (l1, l2) } else { (l2, l1) };
        (alto + 0.05) / (bajo + 0.05)
    }

    for nombre in preset_names() {
        let tema = Theme::preset(nombre)
            .expect("el preset parsea")
            .expect("el preset existe");
        // Sin `background` propio manda el del terminal: no hay contra qué
        // medir (y el tema hereda el emparejamiento del usuario).
        let Some(fondo) = tema.style(Role::Background).bg else {
            continue;
        };
        for role in [Role::Error, Role::Warning, Role::HostileBadge] {
            let Some(fg) = tema.style(role).fg else {
                continue; // sin color: el fallback monocromo hereda el frente
            };
            let r = contraste(fg, fondo);
            assert!(
                r >= 4.5,
                "{nombre}: {role:?} ({}) da {r:.2}:1 sobre el fondo ({}) — WCAG AA pide 4.5",
                fg.to_hex(),
                fondo.to_hex()
            );
        }
    }
}
