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
    "app.palette",
    "app.settings",
    "pane.copy",
    "pane.move",
    "pane.delete",
    "pane.delete-permanent",
    "pane.view",
    "pane.open",
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
    "pane.names-encoding",
];

/// Los comandos del contexto `dialog` (H1, issue #24) — la lista CERRADA
/// que el TUI pasa a [`Effective::build_for`] para `Screen::Dialog`. Cada
/// overlay (modal, theme picker, extensions, nav popup) declara en código
/// su propio ALLOWLIST de cuáles soporta (`app::dialog_action` y las
/// resoluciones ad hoc en `main.rs`); la semántica de seguridad vive ahí,
/// jamás aquí. Coincide 1:1 con las secciones `[dialog]` de los tres
/// presets compartidos (`orthodox`/`vim`/`cua`) — un comando nuevo en el
/// preset sin su entrada aquí falla a construir con `UnknownCommand`.
pub const DIALOG_COMMANDS: &[&str] = &[
    "dialog.confirm",
    "dialog.cancel",
    "dialog.approve",
    "dialog.deny",
    "dialog.overwrite",
    "dialog.skip",
    "dialog.rename",
    "dialog.newer",
    "dialog.up",
    "dialog.down",
    "dialog.page-up",
    "dialog.page-down",
    "dialog.add",
    "dialog.toggle-enabled",
    "dialog.remove",
];

/// Id de Fluent con la descripción de un comando (`app.quit` →
/// `help-cmd-app-quit`). La suite OBLIGA a que exista en ambos locales
/// para TODO comando de [`COMMANDS`]: un comando nuevo sin descripción
/// rompe tests — la ayuda no puede quedarse atrás.
#[must_use]
pub fn help_id(command: &str) -> String {
    format!("help-cmd-{}", command.replace('.', "-"))
}

/// Id de Fluent con la etiqueta CORTA de un comando `dialog.*` (`dialog.
/// page-up` → `dialog-cmd-page-up`), usada por los hints generados de pie
/// de página (H1 T3, #24). Mismo mangling que [`help_id`] (puntos→guiones)
/// aplicado al SUFIJO tras `dialog.` — el prefijo no se repite en el id
/// (evita `dialog-cmd-dialog-page-up`). La suite OBLIGA a que exista en
/// ambos locales para TODO comando de [`DIALOG_COMMANDS`].
#[must_use]
pub fn dialog_hint_id(command: &str) -> String {
    let suffix = command.strip_prefix("dialog.").unwrap_or(command);
    format!("dialog-cmd-{}", suffix.replace('.', "-"))
}

/// Los presets de fábrica, parseados (se validan en tests y al construir
/// el efectivo). Default del producto: `orthodox` (decisión 2026-07-10).
///
/// # Panics
/// Nunca con los TOML embebidos (los valida la suite).
#[must_use]
pub fn presets() -> Vec<(&'static str, KeymapFile)> {
    use norte_frontend::keymap::presets as shared;

    [
        ("orthodox", shared::ORTHODOX),
        ("vim", shared::VIM),
        ("cua", shared::CUA),
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

    /// Decisión #23 pineada: en el preset `cua`, Ctrl+C SALE (emergencia
    /// universal) — jamás copy. `pane.copy` se queda en F5. Ligar Ctrl+C a
    /// copy divergiría del Ctrl-C hardcodeado que aborta el cd/refresh (loops
    /// transitorios que no consultan el keymap). Este test fija la decisión:
    /// quien intente rebindear Ctrl+C a copy rompe aquí y ve el porqué.
    #[test]
    fn cua_ctrl_c_es_salir_no_copy() {
        let cua = presets()
            .into_iter()
            .find(|(n, _)| *n == "cua")
            .expect("preset cua")
            .1;
        let eff = Effective::build_for(&cua, &[], COMMANDS, Screen::Browse).expect("cua efectivo");
        let mut r = Resolver::new(eff);
        let ctrl_c = Chord::new(
            Mods {
                ctrl: true,
                ..Default::default()
            },
            KeyCode::Char('c'),
        );
        assert_eq!(r.push(ctrl_c), Resolution::Run("app.quit".to_owned()));
        // Copy vive en F5, no en un chord de Ctrl.
        assert_eq!(
            r.push(Chord::new(Mods::default(), KeyCode::F(5))),
            Resolution::Run("pane.copy".to_owned())
        );
    }

    /// H1 T2: los tres presets construyen `Screen::Dialog` con el
    /// `known_commands` UNIÓN (`COMMANDS` ∪ `DIALOG_COMMANDS` —
    /// `build_for_impl` valida TODO el efectivo fusionado, incluido
    /// `[global]`, contra la lista que le pasa el caller; T1 lo confirmó).
    /// `y` resuelve `dialog.approve` en los tres (preset idéntico).
    #[test]
    fn dialog_commands_se_resuelven_en_los_tres_presets() {
        let known: Vec<&str> = COMMANDS
            .iter()
            .copied()
            .chain(DIALOG_COMMANDS.iter().copied())
            .collect();
        for (nombre, preset) in presets() {
            let eff = Effective::build_for(&preset, &[], &known, Screen::Dialog)
                .unwrap_or_else(|e| panic!("preset {nombre}: {e}"));
            let mut r = Resolver::new(eff);
            assert_eq!(
                r.push(Chord::new(Mods::default(), KeyCode::Char('y'))),
                Resolution::Run("dialog.approve".to_owned()),
                "preset {nombre}"
            );
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

    #[test]
    fn dialog_hint_id_pela_el_prefijo_dialog_punto() {
        assert_eq!(dialog_hint_id("dialog.confirm"), "dialog-cmd-confirm");
        assert_eq!(dialog_hint_id("dialog.page-up"), "dialog-cmd-page-up");
        assert_eq!(
            dialog_hint_id("dialog.toggle-enabled"),
            "dialog-cmd-toggle-enabled"
        );
    }
}
