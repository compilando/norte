//! Keymap de la TUI: re-exporta el motor compartido de
//! [`norte_frontend::keymap`] y aporta el adaptador de crossterm, la lista
//! de comandos de la TUI y sus presets de fábrica. El motor en sí (tipos
//! neutros, parseo, fusión de capas, resolución) vive en `norte-frontend`
//! (GUI-c T1/T2) — este módulo es una capa fina TUI-específica.
pub use norte_frontend::keymap::{
    Chord, Effective, KeyCode, KeymapError, KeymapFile, Mods, Resolution, Resolver, Screen,
    parse_chord, parse_keymap,
};

use crossterm::event::{KeyCode as CtCode, KeyModifiers as CtMods};

/// Adaptador: evento de crossterm → [`Chord`] neutro. En `Char` el carácter
/// ya codifica shift (lo descarta `Chord::new`); el resto conserva mods.
/// Devuelve `None` para teclas que el keymap no modela (p. ej. `Media`,
/// `BackTab`, `CapsLock`): el caller debe tratarlo como si la tecla no
/// ligara nada (equivalente a [`Resolution::Reset`] — nunca pánico, nunca
/// una tecla "perdida" en silencio distinto de antes).
#[must_use]
pub fn chord_from_crossterm(mods: CtMods, code: CtCode) -> Option<Chord> {
    let neutral = match code {
        CtCode::Char(c) => KeyCode::Char(c),
        CtCode::F(n) => KeyCode::F(n),
        CtCode::Enter => KeyCode::Enter,
        CtCode::Tab => KeyCode::Tab,
        CtCode::Esc => KeyCode::Esc,
        CtCode::Backspace => KeyCode::Backspace,
        CtCode::Up => KeyCode::Up,
        CtCode::Down => KeyCode::Down,
        CtCode::Left => KeyCode::Left,
        CtCode::Right => KeyCode::Right,
        CtCode::Home => KeyCode::Home,
        CtCode::End => KeyCode::End,
        CtCode::PageUp => KeyCode::PageUp,
        CtCode::PageDown => KeyCode::PageDown,
        CtCode::Insert => KeyCode::Insert,
        CtCode::Delete => KeyCode::Delete,
        _ => return None, // teclas que el keymap no modela
    };
    // Solo ctrl/alt/shift: crossterm no reporta super/meta sin
    // PushKeyboardEnhancementFlags (no activado).
    let m = Mods {
        ctrl: mods.contains(CtMods::CONTROL),
        alt: mods.contains(CtMods::ALT),
        shift: mods.contains(CtMods::SHIFT),
    };
    Some(Chord::new(m, neutral))
}

/// Los comandos que el TUI sabe ejecutar — la fuente ÚNICA contra la que
/// se valida todo keymap (los mismos nombres que verán la palette y el
/// wire, ADR 0006).
pub const COMMANDS: &[&str] = &[
    "app.quit",
    "pane.switch",
    "cursor.up",
    "cursor.down",
    "cursor.page-up",
    "cursor.page-down",
    "cursor.top",
    "cursor.bottom",
    "nav.enter",
    "nav.parent",
    "app.help",
    "app.theme",
    "app.extensions",
    "pane.copy",
    "pane.move",
    "pane.delete",
    "pane.delete-permanent",
    "pane.view",
    "task.cancel",
    "viewer.close",
    "viewer.up",
    "viewer.down",
    "viewer.page-up",
    "viewer.page-down",
    "viewer.top",
    "viewer.bottom",
    "viewer.encoding",
    "viewer.encoding-auto",
    "viewer.hex",
    "pane.quick-search",
    "pane.history",
    "pane.hotlist",
    "pane.search",
];

/// Id de Fluent con la descripción de un comando (`app.quit` →
/// `help-cmd-app-quit`). La suite OBLIGA a que exista en ambos locales
/// para TODO comando de [`COMMANDS`]: un comando nuevo sin descripción
/// rompe tests — la ayuda no puede quedarse atrás.
#[must_use]
pub fn help_id(command: &str) -> String {
    format!("help-cmd-{}", command.replace('.', "-"))
}

/// Los presets de fábrica, parseados (se validan en tests y al construir
/// el efectivo). Default del producto: `orthodox` (decisión 2026-07-10).
///
/// # Panics
/// Nunca con los TOML embebidos (los valida la suite).
#[must_use]
pub fn presets() -> Vec<(&'static str, KeymapFile)> {
    [
        ("orthodox", include_str!("keymap_presets/orthodox.toml")),
        ("vim", include_str!("keymap_presets/vim.toml")),
        ("cua", include_str!("keymap_presets/cua.toml")),
    ]
    .into_iter()
    .map(|(name, src)| {
        (
            name,
            parse_keymap(src).unwrap_or_else(|e| panic!("preset {name} embebido inválido: {e}")),
        )
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chord_from_crossterm_traduce_teclas_conocidas() {
        assert_eq!(
            chord_from_crossterm(CtMods::CONTROL, CtCode::Char('k')),
            Some(Chord::new(
                Mods {
                    ctrl: true,
                    ..Default::default()
                },
                KeyCode::Char('k')
            ))
        );
        assert_eq!(
            chord_from_crossterm(CtMods::NONE, CtCode::F(5)),
            Some(Chord::new(Mods::default(), KeyCode::F(5)))
        );
    }

    #[test]
    fn chord_from_crossterm_devuelve_none_para_teclas_no_modeladas() {
        assert_eq!(chord_from_crossterm(CtMods::NONE, CtCode::BackTab), None);
        assert_eq!(chord_from_crossterm(CtMods::NONE, CtCode::CapsLock), None);
    }

    #[test]
    fn los_tres_presets_de_fabrica_parsean() {
        for (nombre, _preset) in presets() {
            assert!(!nombre.is_empty());
        }
    }

    #[test]
    fn help_id_reemplaza_puntos_por_guiones() {
        assert_eq!(help_id("app.quit"), "help-cmd-app-quit");
        assert_eq!(
            help_id("pane.delete-permanent"),
            "help-cmd-pane-delete-permanent"
        );
    }
}
