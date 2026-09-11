//! El hot-reload de la configuración (ADR 0007): relee todas las capas y las
//! aplica TODAS o NINGUNA.
//!
//! Vivía en el root del binario `ntc` —un crate DISTINTO de esta lib—, y es la
//! función con más parámetros de la rama (doce): todo lo que el bucle de
//! eventos retiene y que una recarga puede sustituir. No es API, es cableado,
//! y por eso lleva su `#[expect(clippy::too_many_arguments)]` desde antes de
//! moverse.
//!
//! El criterio que ordena el cuerpo entero: nada se aplica hasta que los tres
//! keymaps se han construido bien. Un TOML a medio guardar —y el watcher los ve
//! a medio guardar— deja la sesión EXACTAMENTE como estaba, con un aviso por la
//! barra.

use std::sync::Arc;

use norte_core::backend::Backend;
use norte_i18n::{t, ta};

use crate::app::{App, config_error_category, keymaps_error_category};
use crate::config::{self, Layers};
use crate::help::TuiChords;
use crate::hints::DialogHints;
use crate::keymap::Resolver;
use crate::nav;
use crate::screens::pickers::apply_theme;
use crate::screens::settings::plugin_config_summaries;
use crate::shortcuts_editor::{Maps, build_keymaps, shortcut_rows};

/// Hot-reload (ADR 0007): relee TODAS las capas; ante CUALQUIER error se
/// conserva la config vigente y se avisa por la barra — jamás romper una
/// sesión en marcha por un TOML a medio guardar.
///
/// Devuelve si la recarga se APLICÓ. El watcher se conforma con el aviso de la
/// barra, pero el cambio de PERFIL (ADR 0079, D8) no: su paso 2 es esta misma
/// recarga con otras capas, y lo que venga detrás —montar la disposición del
/// perfil nuevo, sembrar sus huecos, darlo por activo— solo puede pasar si
/// esto aplicó. Un perfil a medio aplicar no es un estado que ese diseño
/// admita, y el «todo o nada» que esta función ya tenía es justo la semántica
/// que hace falta.
#[expect(clippy::too_many_arguments, reason = "wiring del hot-reload, no API")]
pub async fn reload_config(
    app: &mut App,
    backend: &Backend,
    resolver: &mut Resolver,
    viewer_resolver: &mut Resolver,
    dialog_resolver: &mut Resolver,
    help_lines: &mut Vec<ratatui::text::Line<'static>>,
    // H3b: the negotiated language, so the rebuilt `TuiChords` answers in the
    // same locale it did at startup. Session-fixed (`norte_i18n::force` runs
    // once), so a `[ui] lang` edited in the file does NOT take effect here —
    // the same restriction the rest of the i18n already has.
    lang: norte_i18n::Lang,
    layers: &Layers,
    cli_preset: Option<&str>,
    quick_mode: &mut nav::Mode,
    confirm_quit: &mut config::ConfirmQuit,
    // S3 (`app.settings`): la snapshot COMPLETA que `run()` retiene para
    // construir/refrescar el overlay de ajustes — reemplazada ENTERA solo
    // si TODO el reload aplicó (mismo criterio que el resto de esta
    // función); un reload fallido deja la config VIGENTE, jamás a medias.
    cfg_out: &mut config::LoadedConfig,
) -> bool {
    match config::load_async(layers.clone()).await {
        Ok(cfg) => match build_keymaps(&cfg, cli_preset) {
            Ok((browse, viewer, dialog)) => {
                // El modo del quick search sigue a la config vigente (solo
                // afecta a quick searches NUEVOS; uno abierto conserva el
                // suyo). Mismo criterio que el tema: solo si TODO aplicó.
                *quick_mode = cfg.quick_search_mode;
                // `[ui] confirm_quit` (S2): mismo criterio — solo afecta a
                // `app.quit` NUEVOS (uno ya abierto como `Modal::ConfirmQuit`
                // conserva su decisión hasta que el usuario responda).
                *confirm_quit = cfg.common.ui_confirm_quit;
                app.confirm_quit = cfg.common.ui_confirm_quit;
                // La copia de hotlist también (un popup abierto conserva su
                // snapshot hasta reabrirse — items congelados a propósito).
                app.set_hotlist(cfg.common.hotlist.clone());
                // `[ui] menu_bar` en caliente: el reparto de cada frame lo
                // lee, así que la barra aparece o desaparece en el siguiente
                // pintado —y el ratón la sigue, porque lee ese mismo reparto—.
                app.menu_bar = cfg.common.ui_menu_bar.unwrap_or(true);
                app.panel_bar = cfg.common.ui_panel_bar.unwrap_or(true);
                // El cromo entero, por lo mismo: cada frame lo lee.
                app.chrome = cfg.common.ui_chrome;
                // La fila `..`, también en caliente: es presentación, y el
                // pane la pone o la quita sin tocar el listado.
                app.set_parent_row(cfg.common.ui_parent_entry.unwrap_or(true));
                // Openers (#28): recargados con el resto de la config.
                app.openers = cfg.openers.clone();
                // Y el editor de `[ui] editor`, por lo mismo: quien lo cambia
                // en el fichero no tiene por qué reiniciar norte.
                app.editor = cfg
                    .common
                    .ui_editor
                    .clone()
                    .map(|command| crate::app::EditorSpec {
                        command,
                        detached: cfg.common.ui_editor_detached.unwrap_or(false),
                    });
                // Y el comparador de `[ui] diff` (#312), por lo mismo.
                app.diff = cfg
                    .common
                    .ui_diff
                    .clone()
                    .map(|command| crate::app::EditorSpec {
                        command,
                        detached: cfg.common.ui_diff_detached.unwrap_or(false),
                    });
                // #108 7a: `[ui.columns]` editado fuera también refresca la
                // sesión (antes solo arrancaba); el re-sort mantiene los
                // panes coherentes con el fichero — el persist del picker
                // dispara este mismo camino y es idempotente con lo ya
                // aplicado en memoria.
                app.columns =
                    norte_frontend::columns::ColumnsSettings::resolve(&cfg.common.ui_columns)
                        .with_date_format(cfg.common.ui_chrome.date_format());
                for i in 0..app.panes.len() {
                    app.apply_scheme_sort(i);
                }
                // Bindings `lua:` descartados del keymap de PROYECTO
                // (seguridad — mismo aviso que en el arranque; máximo
                // porque `global` se fusiona en las tres pantallas, H1 T2
                // suma dialog).
                let discarded_lua = browse
                    .discarded_lua_bindings()
                    .max(viewer.discarded_lua_bindings())
                    .max(dialog.discarded_lua_bindings());
                // La ayuda refleja el keymap VIGENTE: se reconstruye aquí.
                *help_lines = crate::help::build(&browse, &viewer, &dialog);
                // H3b: and so does the resolver the CORPUS is rendered
                // through — same effectives, same moment, before they move
                // into the resolvers below (`TuiChords` borrows). A rebind
                // that reached `help_lines` but not this one would leave the
                // generated keyboard page right and every `{{cmd:…}}` mark in
                // the prose teaching the OLD key.
                app.help_chords = Arc::new(TuiChords::new(&browse, &viewer, &dialog, lang));
                app.help = None;
                // Filas de la palette (H1 T4): reconstruidas del keymap
                // VIGENTE, ANTES de que se mueva al resolver de abajo —
                // mismo criterio que help_lines. La palette abierta se
                // cierra (como la ayuda): sus filas congeladas podrían
                // apuntar a descripciones/chords ya viejos.
                app.palette_rows = crate::palette::build_rows(&browse, &viewer);
                app.palette = None;
                // Hints de los overlays (H1 T3, #24): reconstruidos del
                // efectivo `dialog` VIGENTE, ANTES de que se mueva al
                // resolver de abajo — mismo criterio que help_lines.
                app.dialog_hints = DialogHints::build(&dialog);
                app.dialog_hints.buttons = cfg.common.ui_chrome.dialog_buttons();
                // Y la barra de teclas, de los tres (spec 2026-09-10).
                app.key_bars = crate::app::KeyBars::build(&browse, &viewer);
                app.chord_split_h = norte_frontend::palette::first_chord("layout.split-h", &browse);
                // #142: el acorde de vuelta del subshell, del efectivo
                // `browse` VIGENTE y antes de que se mude al resolver —
                // mismo criterio. Un rebind que no llegara aquí dejaría al
                // lector dentro del shell pulsando la tecla nueva.
                app.subshell_chord = norte_frontend::subshell::detach_chord(&browse);
                // K3c: el editor de atajos, si está abierto, se REFRESCA (no
                // se cierra como `help`/`palette`): esta recarga suele ser su
                // propia escritura volviendo por el watcher, y un editor que se
                // cerrase con cada rebind no serviría para el segundo. Sus
                // filas salen de los efectivos VIGENTES, antes de que se muevan
                // a los resolvers — mismo criterio que `help_lines`. La
                // CAPTURA en vuelo, en cambio, no sobrevive: su veredicto se
                // leyó del mapa que se acaba de sustituir
                // (`ShortcutsState::refresh`).
                if let Some(sc) = &mut app.shortcuts {
                    sc.refresh(shortcut_rows(&Maps {
                        browse: &browse,
                        viewer: &viewer,
                        dialog: &dialog,
                    }));
                }
                *resolver = Resolver::new(browse);
                *viewer_resolver = Resolver::new(viewer);
                *dialog_resolver = Resolver::new(dialog);
                // K3a: y con la barra se va el panel which-key — sus filas
                // salieron del efectivo que se acaba de sustituir, así que un
                // panel superviviente enseñaría teclas que ya no existen.
                app.clear_pending();
                app.message = Some(t("msg-config-reloaded"));
                // El tema también es hot-reloadable (ADR 0020): si falla, el
                // mensaje de error del tema pisa el de "config recargada".
                apply_theme(app, &cfg);
                // ÚLTIMO: el aviso de seguridad no debe quedar pisado.
                if discarded_lua > 0 {
                    app.message = Some(ta(
                        "msg-lua-keymap-project",
                        &[("n", &discarded_lua.to_string())],
                    ));
                }
                // S3: el overlay de ajustes, si está abierto, se REFRESCA
                // (no se cierra como `help`/`palette` arriba) — sus filas son
                // solo `(nombre, descripción, value)` leídas de `cfg`, seguras
                // de recomputar sin tirar el filtro/edición en curso del
                // usuario (`Settings::refresh`).
                if let Some(settings) = &mut app.settings {
                    let summaries = plugin_config_summaries(backend).await;
                    settings.refresh(crate::settings::build_rows(&cfg, &summaries));
                }
                *cfg_out = cfg;
                true
            }
            Err(e) => {
                app.message = Some(ta(
                    "msg-config-not-applied",
                    &[("error", &keymaps_error_category(&e))],
                ));
                false
            }
        },
        Err(e) => {
            app.message = Some(ta(
                "msg-config-not-applied",
                &[("error", &config_error_category(&e))],
            ));
            false
        }
    }
}
