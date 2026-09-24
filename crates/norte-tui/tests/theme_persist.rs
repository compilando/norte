//! Persistence of `[ui].theme` from the popup (ADR 0020): writes to the
//! user's norte.toml preserving comments/formatting, creates the file if it
//! does not exist, and is idempotent on reselection.

use norte_tui::config::persist_ui_theme_to;

#[test]
fn creates_the_file_if_it_does_not_exist() {
    let dir = tempfile::tempdir().unwrap();
    let path = persist_ui_theme_to(dir.path(), "nord").unwrap();
    let s = std::fs::read_to_string(&path).unwrap();
    assert!(s.contains("[ui]"));
    assert!(
        s.contains(r#"theme = "nord""#),
        "did not persist the theme: {s}"
    );
}

#[test]
fn preserves_comments_and_other_keys() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("norte.toml");
    std::fs::write(
        &path,
        "# my config\n[ui]\nlang = \"es\"  # language\n\n[keymap]\npreset = \"vim\"\n",
    )
    .unwrap();

    persist_ui_theme_to(dir.path(), "catppuccin-mocha").unwrap();

    let s = std::fs::read_to_string(&path).unwrap();
    assert!(s.contains("# my config"), "lost the header comment");
    assert!(s.contains("# language"), "lost the inline comment");
    assert!(s.contains(r#"lang = "es""#), "lost lang");
    assert!(s.contains(r#"preset = "vim""#), "lost the keymap");
    assert!(
        s.contains(r#"theme = "catppuccin-mocha""#),
        "did not set the theme"
    );
}

#[test]
fn reselecting_updates_in_place() {
    let dir = tempfile::tempdir().unwrap();
    persist_ui_theme_to(dir.path(), "nord").unwrap();
    persist_ui_theme_to(dir.path(), "gruvbox-dark").unwrap();
    let s = std::fs::read_to_string(dir.path().join("norte.toml")).unwrap();
    assert!(s.contains(r#"theme = "gruvbox-dark""#));
    assert!(!s.contains(r#"theme = "nord""#), "the old theme remained");
}
