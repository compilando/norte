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
/// resuelve a `cmd`, si la hay.
fn first_chord(cmd: &str, eff: &Effective) -> Option<String> {
    eff.bindings()
        .into_iter()
        .find(|(_, c)| *c == cmd)
        .map(|(chord, _)| chord)
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
}
