//! Mapeo PURO tecla → acción de navegación (sin GPUI, testeable sin GPU).
//!
//! GPUI entrega un [`gpui::Keystroke`] con `key` = nombre de la tecla física
//! (minúscula para ASCII imprimible: `"a"`, `"tab"`, `"up"`, `"escape"`,
//! `"enter"`, `"backspace"`, `"pageup"`, `"pagedown"`, `"home"`, `"end"`,
//! `"space"` — ver `gpui_linux::linux::platform`). Este módulo traduce ese
//! nombre a una [`Action`] semántica que el handler de `main.rs` ejecuta sobre
//! el [`norte_frontend::PaneState`] con foco. La decisión de si una acción va
//! al quick search o al cursor real la toma el handler según el estado vivo del
//! pane; aquí solo se clasifica la tecla.

/// Acción de navegación derivada de una tecla. `None` = tecla ignorada.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Conmuta el pane con foco (Tab).
    Tab,
    /// Sube (cursor real o selección de quick, decide el handler).
    Up,
    /// Baja.
    Down,
    /// Primera entrada.
    Home,
    /// Última entrada.
    End,
    /// Página arriba.
    PageUp,
    /// Página abajo.
    PageDown,
    /// Carácter imprimible: alimenta el quick search (filter).
    Char(char),
    /// Backspace: borra del filtro si activo, si no sube al padre.
    Backspace,
    /// Escape: cancela el quick search.
    Esc,
    /// Enter: confirma el quick / entra al directorio seleccionado.
    Enter,
    /// Togglea la marca de la entrada bajo el cursor (Insert).
    ToggleMark,
    /// Copia la selección/marcas al pane destino (F5).
    Copy,
    /// Mueve la selección/marcas al pane destino (F6).
    Move,
    /// Borra la selección/marcas (F8/Delete).
    Delete,
    /// Cancela la task seleccionada en la franja (F9).
    CancelTask,
    /// Tecla sin binding.
    None,
}

/// Traduce el nombre de tecla de GPUI a una [`Action`].
///
/// `quick_active` = hay un quick search vivo en el pane con foco. Cambia SOLO
/// el tratamiento de imprimibles: con el filtro abierto CUALQUIER carácter
/// gráfico (incluido el espacio) alimenta la búsqueda; con el filtro cerrado
/// solo un alfanumérico ABRE el filtro (un signo de puntuación o el espacio
/// sueltos son no-op, para no abrir búsquedas por accidente). Las teclas de
/// navegación con nombre mapean igual en ambos casos.
#[must_use]
pub fn key_to_action(key: &str, quick_active: bool) -> Action {
    match key {
        "tab" => Action::Tab,
        "up" => Action::Up,
        "down" => Action::Down,
        "home" => Action::Home,
        "end" => Action::End,
        "pageup" => Action::PageUp,
        "pagedown" => Action::PageDown,
        "backspace" => Action::Backspace,
        "escape" => Action::Esc,
        "enter" => Action::Enter,
        "insert" => Action::ToggleMark,
        "f5" => Action::Copy,
        "f6" => Action::Move,
        "f8" | "delete" => Action::Delete,
        "f9" => Action::CancelTask,
        // El espacio llega con nombre "space", no como carácter suelto.
        "space" => printable(' ', quick_active),
        _ => {
            let mut chars = key.chars();
            match (chars.next(), chars.next()) {
                // Exactamente un carácter: tecla imprimible.
                (Some(c), None) => printable(c, quick_active),
                _ => Action::None,
            }
        }
    }
}

/// Clasifica un carácter imprimible según si el quick search está abierto.
fn printable(c: char, quick_active: bool) -> Action {
    if c.is_control() {
        Action::None
    } else if quick_active {
        // Con el filtro abierto, todo gráfico alimenta la búsqueda.
        Action::Char(c)
    } else if c.is_alphanumeric() {
        // Con el filtro cerrado, solo un alfanumérico lo abre.
        Action::Char(c)
    } else {
        Action::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn teclas_con_nombre_mapean_igual_con_o_sin_quick() {
        for &(key, action) in &[
            ("tab", Action::Tab),
            ("up", Action::Up),
            ("down", Action::Down),
            ("home", Action::Home),
            ("end", Action::End),
            ("pageup", Action::PageUp),
            ("pagedown", Action::PageDown),
            ("backspace", Action::Backspace),
            ("escape", Action::Esc),
            ("enter", Action::Enter),
        ] {
            assert_eq!(key_to_action(key, false), action, "{key} sin quick");
            assert_eq!(key_to_action(key, true), action, "{key} con quick");
        }
    }

    #[test]
    fn alfanumerico_abre_filtro_estando_cerrado() {
        assert_eq!(key_to_action("a", false), Action::Char('a'));
        assert_eq!(key_to_action("z", false), Action::Char('z'));
        assert_eq!(key_to_action("7", false), Action::Char('7'));
    }

    #[test]
    fn puntuacion_y_espacio_no_abren_filtro_cerrado() {
        assert_eq!(key_to_action("-", false), Action::None);
        assert_eq!(key_to_action(".", false), Action::None);
        assert_eq!(key_to_action("space", false), Action::None);
    }

    #[test]
    fn con_filtro_abierto_todo_grafico_alimenta() {
        assert_eq!(key_to_action("-", true), Action::Char('-'));
        assert_eq!(key_to_action(".", true), Action::Char('.'));
        assert_eq!(key_to_action("space", true), Action::Char(' '));
        assert_eq!(key_to_action("a", true), Action::Char('a'));
    }

    #[test]
    fn no_ascii_de_un_solo_caracter_alimenta_el_filtro() {
        // Layouts con letras acentuadas: un solo char imprimible.
        assert_eq!(key_to_action("ñ", true), Action::Char('ñ'));
        assert_eq!(key_to_action("ñ", false), Action::Char('ñ'));
    }

    #[test]
    fn teclas_desconocidas_o_multichar_son_none() {
        assert_eq!(key_to_action("f1", false), Action::None);
        assert_eq!(key_to_action("", true), Action::None);
    }

    #[test]
    fn teclas_de_mutacion_mapean_igual_con_o_sin_quick() {
        for &(key, action) in &[
            ("insert", Action::ToggleMark),
            ("f5", Action::Copy),
            ("f6", Action::Move),
            ("f8", Action::Delete),
            ("delete", Action::Delete),
            ("f9", Action::CancelTask),
        ] {
            assert_eq!(key_to_action(key, false), action, "{key} sin quick");
            assert_eq!(key_to_action(key, true), action, "{key} con quick");
        }
    }
}
