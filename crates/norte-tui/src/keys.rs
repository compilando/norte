//! Mapeo tecla → acción (fase 3: fijo, estilo mc; el keymap engine
//! configurable con presets llega en la fase 4).

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

/// Filas que saltan `PageUp`/`PageDown` (fijo en fase 3; con el keymap
/// engine pasará a depender del alto real del pane).
pub const PAGE: usize = 10;

/// Acción de UI resultante de una tecla.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Salir del TUI.
    Quit,
    /// Cambiar el foco de pane (Tab).
    SwitchFocus,
    /// Subir el cursor `n` filas.
    MoveUp(usize),
    /// Bajar el cursor `n` filas.
    MoveDown(usize),
    /// Cursor al principio del listado.
    Start,
    /// Cursor al final del listado.
    End,
    /// Entrar en la entrada seleccionada.
    Enter,
    /// Subir al directorio padre.
    Parent,
    /// Nada.
    None,
}

/// Traduce el evento. Solo pulsaciones; los modificadores NO se ignoran
/// (Ctrl-C sale — en raw mode no hay SIGINT; Ctrl-Q no es `q`).
#[must_use]
pub fn action_for(key: KeyEvent) -> Action {
    if key.kind != KeyEventKind::Press {
        return Action::None;
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        return match key.code {
            KeyCode::Char('c') => Action::Quit,
            _ => Action::None,
        };
    }
    match key.code {
        KeyCode::Char('q') | KeyCode::F(10) => Action::Quit,
        KeyCode::Tab => Action::SwitchFocus,
        KeyCode::Up => Action::MoveUp(1),
        KeyCode::Down => Action::MoveDown(1),
        KeyCode::PageUp => Action::MoveUp(PAGE),
        KeyCode::PageDown => Action::MoveDown(PAGE),
        KeyCode::Home => Action::Start,
        KeyCode::End => Action::End,
        KeyCode::Enter => Action::Enter,
        KeyCode::Backspace => Action::Parent,
        _ => Action::None,
    }
}
