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

    for name in names {
        let src = preset_source(name).expect("fuente del preset");
        let theme = Theme::from_toml(src).unwrap_or_else(|e| panic!("[{name}] no parsea: {e}"));
        assert_eq!(theme.name.as_deref(), Some(name), "[{name}] name coincide");

        // Cada rol tiene color de frente EXPLÍCITO (un preset colorea todo, no
        // se queda en el fallback monocromo).
        for &role in Role::ALL {
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
