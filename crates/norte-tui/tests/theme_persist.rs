//! Persistencia de `[ui].theme` desde el popup (ADR 0020): escribe en el
//! norte.toml del usuario preservando comentarios/formato, crea el fichero si
//! no existe, y es idempotente al re-elegir.

use norte_tui::config::persist_ui_theme_to;

#[test]
fn crea_el_fichero_si_no_existe() {
    let dir = tempfile::tempdir().unwrap();
    let path = persist_ui_theme_to(dir.path(), "nord").unwrap();
    let s = std::fs::read_to_string(&path).unwrap();
    assert!(s.contains("[ui]"));
    assert!(s.contains(r#"theme = "nord""#), "no persistió el tema: {s}");
}

#[test]
fn preserva_comentarios_y_otras_claves() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("norte.toml");
    std::fs::write(
        &path,
        "# mi config\n[ui]\nlang = \"es\"  # idioma\n\n[keymap]\npreset = \"vim\"\n",
    )
    .unwrap();

    persist_ui_theme_to(dir.path(), "catppuccin-mocha").unwrap();

    let s = std::fs::read_to_string(&path).unwrap();
    assert!(
        s.contains("# mi config"),
        "perdió el comentario de cabecera"
    );
    assert!(s.contains("# idioma"), "perdió el comentario inline");
    assert!(s.contains(r#"lang = "es""#), "perdió lang");
    assert!(s.contains(r#"preset = "vim""#), "perdió el keymap");
    assert!(
        s.contains(r#"theme = "catppuccin-mocha""#),
        "no puso el tema"
    );
}

#[test]
fn re_elegir_actualiza_en_sitio() {
    let dir = tempfile::tempdir().unwrap();
    persist_ui_theme_to(dir.path(), "nord").unwrap();
    persist_ui_theme_to(dir.path(), "gruvbox-dark").unwrap();
    let s = std::fs::read_to_string(dir.path().join("norte.toml")).unwrap();
    assert!(s.contains(r#"theme = "gruvbox-dark""#));
    assert!(!s.contains(r#"theme = "nord""#), "quedó el tema viejo");
}
