//! Tests del mapeo tecla → acción (extraído del binario para poder
//! testearlo; el keymap engine configurable llega en la fase 4).

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use norte_tui::keys::{Action, action_for};

fn press(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
    let mut k = KeyEvent::new(code, mods);
    k.kind = KeyEventKind::Press;
    k
}

#[test]
fn mapeo_basico_estilo_mc() {
    assert_eq!(
        action_for(press(KeyCode::Char('q'), KeyModifiers::NONE)),
        Action::Quit
    );
    assert_eq!(
        action_for(press(KeyCode::F(10), KeyModifiers::NONE)),
        Action::Quit
    );
    assert_eq!(
        action_for(press(KeyCode::Tab, KeyModifiers::NONE)),
        Action::SwitchFocus
    );
    assert_eq!(
        action_for(press(KeyCode::Enter, KeyModifiers::NONE)),
        Action::Enter
    );
    assert_eq!(
        action_for(press(KeyCode::Backspace, KeyModifiers::NONE)),
        Action::Parent
    );
}

#[test]
fn ctrl_c_siempre_sale_y_los_modificadores_no_se_ignoran() {
    assert_eq!(
        action_for(press(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        Action::Quit,
        "Ctrl-C en raw mode llega como tecla: debe salir"
    );
    assert_eq!(
        action_for(press(KeyCode::Char('q'), KeyModifiers::CONTROL)),
        Action::None,
        "Ctrl-Q no es q"
    );
    assert_eq!(
        action_for(press(KeyCode::Char('c'), KeyModifiers::NONE)),
        Action::None
    );
}

#[test]
fn solo_las_pulsaciones_cuentan() {
    let mut k = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
    k.kind = KeyEventKind::Release;
    assert_eq!(action_for(k), Action::None);
}
