//! Filas de la command palette (`Ctrl+P`/vim `:`, H1 T4, spec-promised):
//! mismo criterio que `help::build` (F1) — se construyen del keymap
//! EFECTIVO y del catálogo Fluent `help-cmd-*`, jamás de una lista a mano.
//!
//! [`Row`]/[`plugin_rows`]/[`rows_for_context`]/[`first_chord`] viven en
//! `norte-frontend` (G3c hoist, mismo patrón que [`crate::settings`]) — este
//! módulo los re-exporta para compatibilidad de fuente y añade
//! [`build_rows`], que SÍ es específico del TUI (depende de
//! [`crate::keymap::COMMANDS`]/[`crate::keymap::help_id`], distintos de los
//! de la GUI).

pub use norte_frontend::palette::{
    PLUGIN_DESCRIPTION_WIRE_CAP, Row, first_chord, plugin_rows, rows_for_context,
};
use norte_i18n::t;

use crate::keymap::{COMMANDS, Effective, help_id};

/// Construye las filas de TODOS los comandos de [`COMMANDS`] (browse +
/// viewer comparten el mismo catálogo, ADR 0006): la descripción sale de
/// `help-cmd-*` (la MISMA fuente que F1 — la suite de i18n ya obliga a que
/// exista, `todo_comando_tiene_ayuda_traducida`), el chord es la PRIMERA
/// tecla en precedencia real del efectivo `browse`, o si el comando no
/// vive ahí (es `viewer.*`) la del efectivo `viewer`; sin ninguna, `"—"`
/// (comando válido pero sin tecla en ESTE preset+capas — la palette sigue
/// siendo la única vía para lanzarlo).
#[must_use]
pub fn build_rows(browse: &Effective, viewer: &Effective) -> Vec<Row> {
    COMMANDS
        .iter()
        .map(|&cmd| {
            let desc = t(&help_id(cmd));
            let chord = first_chord(cmd, browse)
                .or_else(|| first_chord(cmd, viewer))
                .unwrap_or_else(|| "—".to_owned());
            Row {
                key: cmd.to_owned(),
                text: cmd.to_owned(),
                desc,
                chord,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::{Screen, presets};

    fn orthodox_effs() -> (Effective, Effective) {
        let (_, preset) = presets()
            .into_iter()
            .find(|(n, _)| *n == "orthodox")
            .expect("preset orthodox");
        let browse = Effective::build_for(&preset, &[], COMMANDS, Screen::Browse).unwrap();
        let viewer = Effective::build_for(&preset, &[], COMMANDS, Screen::Viewer).unwrap();
        (browse, viewer)
    }

    #[test]
    fn build_rows_una_fila_por_comando_con_chord_de_browse() {
        let (browse, viewer) = orthodox_effs();
        let rows = build_rows(&browse, &viewer);
        assert_eq!(rows.len(), COMMANDS.len(), "una fila por comando, sin más");
        let quit = rows.iter().find(|r| r.key == "app.quit").unwrap();
        assert_eq!(quit.key, quit.text, "built-in: key == text (confiable)");
        assert_eq!(quit.desc, "quit norte");
        // Ligado en [global]: q/f10/ctrl+c — la PRIMERA en precedencia.
        assert_ne!(quit.chord, "—");
    }

    #[test]
    fn build_rows_cae_a_viewer_si_no_esta_en_browse() {
        let (browse, viewer) = orthodox_effs();
        let rows = build_rows(&browse, &viewer);
        let close = rows.iter().find(|r| r.key == "viewer.close").unwrap();
        assert_ne!(close.chord, "—", "viewer.close vive en Screen::Viewer");
    }

    /// Encoding audit H1: mismo defecto que `dialog_hints` pero en la
    /// columna chord de la command palette (`build_rows`/`first_chord`) — un
    /// chord hostil de una capa de usuario/proyecto rebindeado a un comando
    /// de browse (`pane.copy`, siempre presente en `COMMANDS`) no debe
    /// pintarse crudo.
    #[test]
    fn build_rows_enmascara_chords_hostiles_en_la_columna_chord() {
        let (_, preset) = presets()
            .into_iter()
            .find(|(n, _)| *n == "orthodox")
            .expect("preset orthodox");
        let viewer_vacio = Effective::build_for(&preset, &[], COMMANDS, Screen::Viewer).unwrap();
        for hazard in norte_testkit::corpus::hostile_chords() {
            let token_esc = format!("\\u{:04X}", hazard.token as u32);
            let layer_src = format!(
                r#"
                [pane]
                prepend_keymap = [{{ on = ["{token_esc}"], run = "pane.copy" }}]
                "#,
            );
            let layer = crate::keymap::parse_keymap(&layer_src).unwrap();
            let browse = Effective::build_for(&preset, &[layer], COMMANDS, Screen::Browse)
                .unwrap_or_else(|e| panic!("[{}] keymap efectivo: {e}", hazard.id));
            let rows = build_rows(&browse, &viewer_vacio);
            let copy = rows
                .iter()
                .find(|r| r.key == "pane.copy")
                .unwrap_or_else(|| panic!("[{}] fila pane.copy", hazard.id));
            assert!(
                !copy.chord.chars().any(norte_encoding::is_terminal_hazard),
                "[{}] hazard crudo en la columna chord: {:?}",
                hazard.id,
                copy.chord
            );
            assert!(
                copy.chord.contains('\u{FFFD}'),
                "[{}] el hazard debe enmascararse a U+FFFD: {:?}",
                hazard.id,
                copy.chord
            );
        }
    }

    /// MINOR-6 (H1 close): abierta desde BROWSE (`viewer_open = false`), la
    /// palette oculta `viewer.*` — despacharla sin `app.viewer` sería un
    /// no-op silencioso.
    #[test]
    fn rows_for_context_oculta_viewer_desde_browse() {
        let (browse, viewer) = orthodox_effs();
        let rows = build_rows(&browse, &viewer);
        let filtradas = rows_for_context(&rows, false);
        assert!(
            filtradas.iter().all(|r| !r.key.starts_with("viewer.")),
            "ninguna fila viewer.* debería sobrevivir al filtrado desde browse"
        );
        assert!(
            filtradas.iter().any(|r| r.key.starts_with("pane.")),
            "las filas pane.* siguen presentes"
        );
        assert!(
            filtradas.len() < rows.len(),
            "el filtrado debe quitar AL MENOS las filas viewer.*"
        );
    }

    /// Abierta DESDE el viewer (`viewer_open = true`), la palette conserva
    /// TODO — incluidas las filas `pane.*`.
    #[test]
    fn rows_for_context_mantiene_todo_desde_el_viewer() {
        let (browse, viewer) = orthodox_effs();
        let rows = build_rows(&browse, &viewer);
        assert_eq!(rows_for_context(&rows, true), rows);
    }
}
