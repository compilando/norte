//! El popup selector de tema: navegación con preview EN VIVO, Enter fija,
//! Esc revierte (ADR 0020). La lógica vive en `App`, así que se testea sin el
//! bucle de eventos.

use norte_proto::VPath;
use norte_tui::app::{App, Pane, PickerAction};
use norte_tui::ui;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

fn vp(w: &str) -> VPath {
    let _ = norte_i18n::force(norte_i18n::Lang::Es);
    VPath::parse(w).expect("wire")
}

fn app() -> App {
    let d = vp("file:///x");
    App::new(Pane::new(d.clone(), Vec::new()), Pane::new(d, Vec::new()))
}

#[test]
fn abrir_lista_los_presets_y_previsualiza() {
    let mut app = app();
    app.open_theme_picker();
    let picker = app.theme_picker.as_ref().expect("popup abierto");
    // Todos los presets embebidos están.
    assert!(picker.names.iter().any(|n| n == "catppuccin-mocha"));
    assert!(picker.names.iter().any(|n| n == "nord"));
    // El popup se pinta (el título y los nombres salen en el buffer).
    let mut t = Terminal::new(TestBackend::new(60, 16)).expect("term");
    t.draw(|f| ui::draw(f, &app)).expect("draw");
    let text = t.backend().to_string();
    assert!(
        text.contains("nord"),
        "el popup no lista los temas: {text}"
    );
}

#[test]
fn navegar_previsualiza_y_enter_fija() {
    let mut app = app();
    app.open_theme_picker();
    // Muévete hasta catppuccin-mocha y confirma.
    // (El orden de names viene de preset_names; navegamos hasta encontrarlo.)
    let idx = app
        .theme_picker
        .as_ref()
        .unwrap()
        .names
        .iter()
        .position(|n| n == "catppuccin-mocha")
        .unwrap();
    for _ in 0..idx {
        app.theme_picker_input(PickerAction::Down);
    }
    // Preview EN VIVO: navegar ya cambió el tema vigente (el color exacto,
    // dependiente de la profundidad del terminal, lo cubre theme_render.rs).
    assert_eq!(
        app.theme.name(),
        Some("catppuccin-mocha"),
        "el preview no aplicó"
    );
    app.theme_picker_input(PickerAction::Confirm);
    // Cerrado y FIJADO: el tema sigue siendo catppuccin tras confirmar.
    assert!(app.theme_picker.is_none());
    assert_eq!(app.theme.name(), Some("catppuccin-mocha"));
}

#[test]
fn cancelar_revierte_al_tema_previo() {
    let mut app = app();
    let before = app.theme.name().map(String::from);
    app.open_theme_picker();
    app.theme_picker_input(PickerAction::Down);
    app.theme_picker_input(PickerAction::Down);
    // Cancela: vuelve al tema de antes de abrir.
    app.theme_picker_input(PickerAction::Cancel);
    assert!(app.theme_picker.is_none());
    assert_eq!(app.theme.name().map(String::from), before);
}
