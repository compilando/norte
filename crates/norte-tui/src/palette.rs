//! Filas de la command palette (`Ctrl+P`/vim `:`, H1 T4, spec-promised):
//! mismo criterio que `help::build` (F1) — se construyen del keymap
//! EFECTIVO y del catálogo Fluent `help-cmd-*`, jamás de una lista a mano.
//! A diferencia de la ayuda (que solo LISTA), la palette necesita el
//! nombre crudo del comando (`&'static str`, para despacharlo al Enter),
//! así que vive en su propio módulo — `app::Palette` consume estas filas,
//! no las calcula.

use norte_i18n::t;

use crate::keymap::{COMMANDS, Effective, help_id};

/// Una fila: `(comando, descripción YA traducida, chord-o-guion)`.
pub type Row = (&'static str, String, String);

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
            (cmd, desc, chord)
        })
        .collect()
}

/// La PRIMERA chord (en el orden de precedencia de `eff.bindings()`) que
/// resuelve a `cmd`, si la hay. RENDER-side duty (encoding audit H1): `eff`
/// puede venir de un keymap hostil (`./.norte/keymap.toml`, capa de
/// PROYECTO sin trust — `parse_chord` acepta CUALQUIER codepoint suelto);
/// `Chord`'s `Display` lo escribe crudo A PROPÓSITO (logs/debug quieren el
/// chord real), así que la columna chord de la palette se enmascara aquí,
/// no en el motor — mismo mecanismo que `hints::dialog_hints`.
fn first_chord(cmd: &str, eff: &Effective) -> Option<String> {
    eff.bindings()
        .into_iter()
        .find(|(_, c)| *c == cmd)
        .map(|(chord, _)| norte_encoding::mask_terminal_hazards(&chord))
}

/// Filtra la snapshot completa de [`build_rows`] para una palette ABIERTA en
/// un contexto dado (MINOR-6, H1 close). `Ctrl+P`/vim `:` viven en
/// `[global]`, que se funde en AMBOS efectivos (`Screen::Browse` y
/// `Screen::Viewer`, ver `merge_ctx`) — la palette puede abrirse desde el
/// viewer, no solo desde browse. Un `viewer.*` despachado SIN viewer abierto
/// es un no-op silencioso (`main::dispatch` los resuelve contra
/// `app.viewer`, que sería `None`), así que se ocultan cuando NO hay viewer.
/// Abierta DESDE el viewer conserva TODAS las filas — `pane.*` sigue
/// alcanzando el pane con foco igual (el viewer no lo sustituye); un
/// filtrado simétrico (ocultar `pane.*` desde el viewer) queda para cuando
/// la palette sea consciente de pantalla en ambos sentidos.
#[must_use]
pub fn rows_for_context(rows: &[Row], viewer_open: bool) -> Vec<Row> {
    if viewer_open {
        rows.to_vec()
    } else {
        rows.iter()
            .filter(|(cmd, ..)| !cmd.starts_with("viewer."))
            .cloned()
            .collect()
    }
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
        let quit = rows.iter().find(|(cmd, ..)| *cmd == "app.quit").unwrap();
        assert_eq!(quit.1, "quit norte");
        // Ligado en [global]: q/f10/ctrl+c — la PRIMERA en precedencia.
        assert_ne!(quit.2, "—");
    }

    #[test]
    fn build_rows_cae_a_viewer_si_no_esta_en_browse() {
        let (browse, viewer) = orthodox_effs();
        let rows = build_rows(&browse, &viewer);
        let close = rows
            .iter()
            .find(|(cmd, ..)| *cmd == "viewer.close")
            .unwrap();
        assert_ne!(close.2, "—", "viewer.close vive en Screen::Viewer");
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
                .find(|(cmd, ..)| *cmd == "pane.copy")
                .unwrap_or_else(|| panic!("[{}] fila pane.copy", hazard.id));
            assert!(
                !copy.2.chars().any(norte_encoding::is_terminal_hazard),
                "[{}] hazard crudo en la columna chord: {:?}",
                hazard.id,
                copy.2
            );
            assert!(
                copy.2.contains('\u{FFFD}'),
                "[{}] el hazard debe enmascararse a U+FFFD: {:?}",
                hazard.id,
                copy.2
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
            filtradas
                .iter()
                .all(|(cmd, ..)| !cmd.starts_with("viewer.")),
            "ninguna fila viewer.* debería sobrevivir al filtrado desde browse"
        );
        assert!(
            filtradas.iter().any(|(cmd, ..)| cmd.starts_with("pane.")),
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
