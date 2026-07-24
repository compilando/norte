//! Filas de la command palette (`Ctrl+P`/vim `:`, H1 T4, spec-promised):
//! mismo criterio que `help::build` (F1) — se construyen del keymap
//! EFECTIVO y del catálogo Fluent `help-cmd-*`, jamás de una lista a mano.
//! A diferencia de la ayuda (que solo LISTA), la palette necesita el
//! nombre crudo del comando (para despacharlo al Enter), así que vive en su
//! propio módulo — `app::Palette` consume estas filas, no las calcula.
//!
//! P1 añade [`plugin_rows`]: filas de comandos de plugin, mezcladas con las
//! de [`build_rows`] en `main::dispatch`'s brazo `"app.palette"`. Un
//! comando de plugin trae texto de TERCEROS (título, descripción del
//! manifiesto) — a diferencia de las filas built-in (siempre confiables:
//! constantes del binario y catálogo Fluent), así que [`Row`] separa la
//! clave de despacho (`key`, jamás se pinta) del texto a mostrar
//! (`text`/`desc`, YA enmascarado si hace falta).

use norte_i18n::t;

use crate::keymap::{COMMANDS, Effective, help_id};

/// Una fila de la palette.
///
/// `key` es la clave de DESPACHO interna, consumida por `main::dispatch` al
/// pulsar Enter — JAMÁS se pinta. Para un comando built-in ([`build_rows`])
/// `key == text`: ambos son texto CONFIABLE (constante del binario). Para un
/// comando de plugin ([`plugin_rows`], P1) `key` codifica
/// `plugin:{plugin_id}:{command_id}`: el `plugin_id` es reverse-DNS
/// charset-validado por el core (nunca lleva `:`), pero el `command_id` del
/// manifiesto NO tiene charset validado (a diferencia del id del propio
/// plugin) — puede llevar cualquier byte, incluidos `:` o saltos de línea.
/// El primer `:` que sigue al prefijo `plugin:` + `plugin_id` separa ambos
/// sin ambigüedad (el `plugin_id` no puede contener uno), y el resto —
/// TODO lo que quede, sin volver a partir— es el `command_id` crudo. Por
/// eso `key` nunca se pinta: `text`/`desc` son la vista YA enmascarada
/// ([`crate::app::display_name`]) del título/descripción del plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// Clave de despacho — NUNCA se pinta (ver doc de arriba).
    pub key: String,
    /// Texto a mostrar (comando built-in, o título de plugin YA
    /// enmascarado).
    pub text: String,
    /// Descripción a mostrar (ayuda Fluent, o descripción de plugin YA
    /// enmascarada).
    pub desc: String,
    /// Chord real, o `"—"` si el comando no tiene tecla en este preset (o,
    /// para un comando de plugin, siempre — la palette es su única vía).
    pub chord: String,
}

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

/// Filas de comandos de plugin (P1): SOLO plugins APROBADOS y ACTIVADOS —
/// el mismo gate que exige `plugin.run_command` (norte-core `plugins.rs`),
/// para que la palette jamás ofrezca invocar algo que el backend rechazaría.
/// Cada [`norte_proto::methods::PluginCommandInfo`] del catálogo se
/// convierte en una fila, EN ORDEN DE MANIFIESTO (mismo criterio que el
/// gestor de extensiones: no se reordena). `text` (título) y `desc`
/// (descripción del PLUGIN, la misma para todos sus comandos) son texto de
/// TERCEROS — se enmascaran con [`crate::app::display_name`] antes de
/// entrar en la fila (nunca en el punto de pintado, igual que `first_chord`
/// enmascara la columna chord aquí y no en el motor) y `text` lleva el
/// prefijo `palette-plugin-prefix` (p. ej. `[extension] …`): un plugin
/// hostil podría titular su comando IGUAL que un comando built-in
/// (`"app.quit"`) para confundir al humano — el prefijo, más el hecho de
/// que un built-in jamás lo lleva, rompe el disfraz.
#[must_use]
pub fn plugin_rows(plugins: &[norte_proto::methods::PluginInfo]) -> Vec<Row> {
    plugins
        .iter()
        .filter(|p| p.approved && p.enabled)
        .flat_map(|p| {
            let desc = p
                .description
                .as_deref()
                .map(|d| crate::app::display_name(d.as_bytes()).0)
                .unwrap_or_default();
            let plugin_id = p.id.clone();
            p.commands.iter().map(move |c| {
                let (title, _) = crate::app::display_name(c.title.as_bytes());
                Row {
                    key: format!("plugin:{plugin_id}:{}", c.id),
                    text: format!("[{}] {title}", t("palette-plugin-prefix")),
                    desc: desc.clone(),
                    chord: "—".to_owned(),
                }
            })
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
            .filter(|row| !row.key.starts_with("viewer."))
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

    fn plugin_info(
        id: &str,
        approved: bool,
        enabled: bool,
        description: Option<&str>,
        commands: Vec<(&str, &str)>,
    ) -> norte_proto::methods::PluginInfo {
        norte_proto::methods::PluginInfo {
            id: id.into(),
            name: "N".into(),
            publisher: "p".into(),
            version: "0.1.0".into(),
            category: "command".into(),
            capabilities: Vec::new(),
            approved,
            enabled,
            description: description.map(str::to_owned),
            commands: commands
                .into_iter()
                .map(|(cid, title)| norte_proto::methods::PluginCommandInfo {
                    id: cid.into(),
                    title: title.into(),
                })
                .collect(),
        }
    }

    #[test]
    fn plugin_rows_solo_aprobados_y_activados() {
        let plugins = vec![
            plugin_info("org.a", true, true, None, vec![("greet", "Greet")]),
            plugin_info("org.b", false, true, None, vec![("x", "X")]),
            plugin_info("org.c", true, false, None, vec![("y", "Y")]),
        ];
        let rows = plugin_rows(&plugins);
        assert_eq!(rows.len(), 1, "solo org.a está aprobado Y activado");
        assert_eq!(rows[0].key, "plugin:org.a:greet");
    }

    #[test]
    fn plugin_rows_una_fila_por_comando_en_orden_de_manifiesto() {
        let plugins = vec![plugin_info(
            "org.norte.demo",
            true,
            true,
            Some("Saluda desde la palette."),
            vec![("greet", "Greet"), ("wave", "Wave")],
        )];
        let rows = plugin_rows(&plugins);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].key, "plugin:org.norte.demo:greet");
        assert!(rows[0].text.contains("Greet"));
        assert_eq!(rows[0].desc, "Saluda desde la palette.");
        assert_eq!(rows[1].key, "plugin:org.norte.demo:wave");
        assert_eq!(rows[1].chord, "—");
    }

    #[test]
    fn plugin_rows_sin_description_es_desc_vacia() {
        let plugins = vec![plugin_info(
            "org.norte.demo",
            true,
            true,
            None,
            vec![("greet", "Greet")],
        )];
        let rows = plugin_rows(&plugins);
        assert_eq!(rows[0].desc, "");
    }

    /// P1, encoding audit: un título de plugin hostil (bidi override, corpus
    /// `rtl_override`) NUNCA se pinta crudo — `text` lo lleva por
    /// `display_name` ANTES de llegar a la fila (mismo criterio que el
    /// gestor de extensiones y que la columna chord de `build_rows`).
    #[test]
    fn plugin_rows_enmascara_titulo_hostil() {
        let hostil = norte_testkit::corpus::hostile_names()
            .into_iter()
            .find(|n| n.id == "rtl_override")
            .expect("fixture del corpus");
        let titulo = String::from_utf8_lossy(&hostil.bytes).into_owned();
        let plugins = vec![plugin_info(
            "org.evil.x",
            true,
            true,
            None,
            vec![("run", titulo.as_str())],
        )];
        let rows = plugin_rows(&plugins);
        assert_eq!(rows.len(), 1);
        assert!(
            !rows[0].text.chars().any(norte_encoding::is_terminal_hazard),
            "el override RTL se pintó crudo: {:?}",
            rows[0].text
        );
        assert!(
            rows[0].text.contains('\u{FFFD}'),
            "el hazard debe enmascararse a U+FFFD: {:?}",
            rows[0].text
        );
        // El KEY (despacho interno) jamás se pinta — puede llevar el
        // command_id crudo, sin validar, tal cual el manifiesto lo declaró.
        assert_eq!(rows[0].key, "plugin:org.evil.x:run");
    }

    /// El prefijo `[extension]` (P1) marca visualmente una fila de plugin —
    /// un plugin no puede disfrazarse de comando built-in copiando su
    /// nombre exacto, porque ningún built-in lleva este prefijo.
    #[test]
    fn plugin_rows_llevan_el_prefijo_de_extension() {
        let plugins = vec![plugin_info(
            "org.norte.demo",
            true,
            true,
            None,
            vec![("greet", "Greet")],
        )];
        let rows = plugin_rows(&plugins);
        assert!(
            rows[0].text.starts_with('['),
            "fila de plugin sin prefijo: {:?}",
            rows[0].text
        );
    }
}
