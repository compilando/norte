//! El mapa de disco en la TUI: el kind y sus teclas.
//!
//! El estado vive en [`norte_frontend::diskmap`], no aquí, por lo mismo que el
//! panel de procesos: «cuál es el hijo elegido» es una pregunta que contestan
//! las dos superficies, y una decisión escrita dos veces diverge en silencio
//! (ADR 0077). Lo que queda en este crate es el `KIND` —que es lo que escribe
//! la disposición— y qué tecla hace qué mientras el panel tiene el teclado.

pub use norte_frontend::diskmap::{DiskMap, Estado};

/// El kind que ocupa un hueco de mapa de disco.
pub const KIND: &str = "disk-map";

/// Lo que una tecla le pide al mapa.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapAction {
    /// Mover la elección `n` rectángulos.
    Mover(isize),
    /// Entrar en el hijo elegido.
    Entrar,
    /// Volver a medir este directorio.
    Remedir,
    /// Devolver el teclado sin cerrar el panel.
    Leave,
}

/// Traduce una tecla del mapa de disco.
///
/// Un `match` explícito y NO el keymap, con el mismo criterio que el panel de
/// registro: estas teclas solo existen mientras el mapa tiene el teclado, y
/// meterlas en el keymap obligaría a los siete presets a declarar atajos que
/// fuera de aquí no significan nada.
///
/// `r` de «remedir» es la única letra suelta, y se gana su sitio: un mapa es
/// una foto de hace un rato, y volver a medir sin salir del panel es lo que
/// uno quiere justo después de borrar algo grande.
#[must_use]
pub fn key(
    code: crossterm::event::KeyCode,
    mods: crossterm::event::KeyModifiers,
) -> Option<MapAction> {
    use crossterm::event::{KeyCode, KeyModifiers};
    if !(mods.is_empty() || mods == KeyModifiers::SHIFT) {
        return None;
    }
    Some(match code {
        KeyCode::Down | KeyCode::Right => MapAction::Mover(1),
        KeyCode::Up | KeyCode::Left => MapAction::Mover(-1),
        // Las páginas se mueven por bloques, como en cualquier lista: un mapa
        // de 4096 rectángulos no se recorre de uno en uno.
        KeyCode::PageDown => MapAction::Mover(PAGINA),
        KeyCode::PageUp => MapAction::Mover(-PAGINA),
        KeyCode::Home => MapAction::Mover(isize::MIN),
        KeyCode::End => MapAction::Mover(isize::MAX),
        KeyCode::Enter => MapAction::Entrar,
        KeyCode::Char('r') => MapAction::Remedir,
        KeyCode::Esc => MapAction::Leave,
        _ => return None,
    })
}

/// Cuántos rectángulos salta una página.
const PAGINA: isize = 10;

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyModifiers};

    /// Las cuatro flechas mueven: en un mapa no hay «arriba» y «al lado», hay
    /// una lista de rectángulos que se recorre.
    #[test]
    fn las_cuatro_flechas_mueven() {
        for (code, esperado) in [
            (KeyCode::Down, MapAction::Mover(1)),
            (KeyCode::Right, MapAction::Mover(1)),
            (KeyCode::Up, MapAction::Mover(-1)),
            (KeyCode::Left, MapAction::Mover(-1)),
        ] {
            assert_eq!(key(code, KeyModifiers::NONE), Some(esperado));
        }
    }

    /// Enter entra, `r` vuelve a medir, `Esc` suelta el teclado SIN cerrar.
    #[test]
    fn las_teclas_propias_del_mapa() {
        assert_eq!(
            key(KeyCode::Enter, KeyModifiers::NONE),
            Some(MapAction::Entrar)
        );
        assert_eq!(
            key(KeyCode::Char('r'), KeyModifiers::NONE),
            Some(MapAction::Remedir)
        );
        assert_eq!(
            key(KeyCode::Esc, KeyModifiers::NONE),
            Some(MapAction::Leave)
        );
    }

    /// Con Ctrl o Alt no es del mapa: esas van al keymap, que es donde vive
    /// `alt+z` para cerrarlo y `alt+l` para abrir el de al lado.
    #[test]
    fn con_ctrl_o_alt_la_tecla_no_es_del_mapa() {
        assert!(key(KeyCode::Char('r'), KeyModifiers::CONTROL).is_none());
        assert!(key(KeyCode::Down, KeyModifiers::ALT).is_none());
    }

    /// Una letra cualquiera no hace nada: el mapa no se come el alfabeto.
    #[test]
    fn una_letra_ajena_no_es_del_mapa() {
        assert!(key(KeyCode::Char('q'), KeyModifiers::NONE).is_none());
    }
}
