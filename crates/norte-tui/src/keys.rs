//! El enrutado de una tecla: quién se la queda.
//!
//! Es una CADENA de precedencia, y el orden es la especificación: el menú y
//! los selectores mandan sobre el listado, un modal manda sobre casi todo
//! (`modal_wins`), y lo que no reclama nadie cae al resolver del keymap, que
//! es quien convierte un acorde en un [`Command`].
//!
//! Vivía dentro de `run`, y por eso `run` tenía dos mil doscientas líneas.
//! Sale entera porque no depende del `select!`: recibe el trabajo en vuelo
//! ([`InFlight`]) y lo que la recarga en caliente puede sustituir, y devuelve
//! cuando la tecla está servida —los `continue` del bucle son aquí `return`,
//! que es lo mismo dicho sin bucle.

use crate::app::{App, Modal, PAGE, Palette, PromptKind, error_message};
use crate::config::{self};
use crate::dispatch::dispatch;
use crate::event_loop::{launch_pending, run_command};
use crate::gestures::{keyboard_owner, submit_command_line};
use crate::jobs::{
    AiRenameRun, InFlight, RenameBatchRun, SemanticRun, launch_search, on_compare_key,
    on_search_dialog_key, on_search_enter, on_search_escape, on_sync_key,
};
use crate::keymap::{
    Command, Count, Resolution, Resolver, chord_from_crossterm, count_ignored_message,
    parse_plugin_key, unavailable_message,
};
use crate::lua::{resolve_lua_trust, run_lua_command};
use crate::mutations::{on_dialog_key, submit_transfer};
use crate::nav;
use crate::navigate::{apply_cd, cd, settle_cd};
use crate::overlays::{close_stale_overlays, help_owns_keys, modal_wins, palette_help};
use crate::refresh::{after_panes_refresh, reap_search_run, refresh_panes};
use crate::screens::{
    HelpDispatch, on_columns_key, on_connections_picker_key, on_disk_map_key, on_extensions_key,
    on_help_key, on_layout_picker_key, on_nav_popup_key, on_panel_key, on_places_key,
    on_processes_key, on_profile_picker_key, on_settings_key, on_theme_picker_key, on_tree_key,
    run_plugin_command,
};
use crate::shortcuts_editor::{Maps, on_shortcuts_key};
use crate::trail::{nav_enter_target, nav_stalled};
use crossterm::event::KeyEvent;
use crossterm::event::{KeyCode, KeyModifiers};
use norte_core::TransferOptions;
use norte_core::backend::Backend;
use norte_frontend::SEMANTIC_K;
use norte_i18n::{t, ta};
use norte_proto::VPath;

/// Enruta una tecla por la cadena de precedencia. Ver el módulo.
#[expect(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "wiring del bucle, no API"
)]
pub async fn on_key(
    app: &mut App,
    backend: &Backend,
    capture: &mut crate::mouse::Capture,
    // La terminal viaja DENTRO (`events.terminal()`): tenía que tener un solo
    // dueño para que una espera larga pudiera repintarse, y ese dueño es la
    // consola.
    events: &mut crate::console::Console<'_>,
    resolver: &mut Resolver,
    viewer_resolver: &mut Resolver,
    dialog_resolver: &mut Resolver,
    help_lines: &mut Vec<ratatui::text::Line<'static>>,
    lang: norte_i18n::Lang,
    quick_mode: nav::Mode,
    confirm_quit: config::ConfirmQuit,
    cfg: &config::LoadedConfig,
    cli_preset: Option<&str>,
    lua_host: Option<&crate::lua::LuaHost>,
    work: &mut InFlight,
    key: KeyEvent,
) {
    app.message = None;
    if app.menu.is_some() && !modal_wins(app) {
        // La barra de menús: teclas FIJAS, como la
        // palette. No hay verbos `dialog.*` para
        // «siguiente menú», así que tampoco pueden
        // salir del keymap.
        let plain = key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT;
        match key.code {
            KeyCode::Esc if plain => app.close_menu(),
            KeyCode::Left if plain => {
                if let Some(m) = &mut app.menu {
                    m.cycle_menu(-1);
                }
            }
            KeyCode::Right if plain => {
                if let Some(m) = &mut app.menu {
                    m.cycle_menu(1);
                }
            }
            KeyCode::Up if plain => {
                if let Some(m) = &mut app.menu {
                    m.cycle_item(-1);
                }
            }
            KeyCode::Down if plain => {
                if let Some(m) = &mut app.menu {
                    m.cycle_item(1);
                }
            }
            KeyCode::Enter if plain => {
                // El menú se CIERRA antes de despachar,
                // por lo mismo que la palette: el
                // comando puede abrir otro overlay, y
                // hacerlo por detrás de este dejaría el
                // menú comiéndose las teclas del que
                // acaba de abrirse.
                if let Some(id) = app.take_menu_choice()
                    && let Some(cmd) = Command::parse(&id)
                {
                    // MISMO camino que la palette y que
                    // el resolver: un comando elegido en
                    // un menú corre exactamente como si
                    // se hubiera pulsado su tecla.
                    //
                    // El cuerpo está duplicado del brazo
                    // de la palette a sabiendas:
                    // extraerlo pide una función de doce
                    // parámetros —`events`,
                    // `terminal`, `capture`— o refactorizar
                    // el run loop, y ninguna de las dos
                    // cabe en el cambio que trae el menú.
                    run_command(
                        app,
                        backend,
                        events,
                        help_lines,
                        lang,
                        quick_mode,
                        confirm_quit,
                        cfg,
                        &mut work.fill,
                        &mut work.decorate,
                        &mut work.probed,
                        &mut work.search,
                        cmd,
                    )
                    .await;
                    launch_pending(app, events, capture).await;
                }
            }
            _ => {}
        }
    } else if app.theme_picker.is_some() && !modal_wins(app) {
        on_theme_picker_key(app, dialog_resolver, key.modifiers, key.code).await;
    } else if app.connections_picker.is_some() && !modal_wins(app) {
        // #140: confirmar devuelve la URL y navegar es
        // un `cd` como cualquier otro — con su ritual
        // de vuelta, para que el drenador paginado y
        // las sondas del pane anterior no sigan vivos.
        if let Some(url) = on_connections_picker_key(app, dialog_resolver, key.modifiers, key.code)
        {
            match VPath::parse(&url) {
                Ok(destino) => {
                    let outcome = cd(app, backend, events, destino).await;
                    apply_cd(
                        &app.panes,
                        &mut work.fill,
                        &mut work.decorate,
                        &mut work.probed,
                        &mut work.search,
                        outcome,
                    );
                }
                Err(_) => {
                    app.message = Some(ta(
                        "msg-connect-bad-url",
                        &[("url", &norte_encoding::mask_terminal_hazards(&url))],
                    ));
                }
            }
        }
    } else if app.layout_picker.is_some() && !modal_wins(app) {
        // Mismo puesto en la cadena y el MISMO
        // allowlist que el selector de tema: los dos
        // son una lista con cursor que no muta datos.
        on_layout_picker_key(app, dialog_resolver, key.modifiers, key.code);
    } else if app.profile_picker.is_some() && !modal_wins(app) {
        // El de perfiles, en el mismo puesto de la cadena y con el mismo
        // allowlist: es otra lista con cursor que no muta datos. Confirmar
        // deja el cambio PEDIDO y lo hace el bucle.
        on_profile_picker_key(app, dialog_resolver, key.modifiers, key.code);
    } else if app.columns_picker.is_some() && !modal_wins(app) {
        // Picker de columnas (#108 7a): mismo puesto en la
        // cadena que el selector de tema (overlay antes que
        // el brazo del modal, precedencia existente).
        if on_columns_key(app, dialog_resolver, key.modifiers, key.code).await {
            // #117: el set de attrs pintado cambió — los
            // valores solo llegan pidiéndolos, así que se
            // re-lista por el MISMO camino que tras una
            // mutación (ritual en `after_panes_refresh`).
            let refreshed = refresh_panes(app, backend, events).await;
            after_panes_refresh(
                app,
                refreshed,
                &mut work.fill,
                &mut work.probed,
                &mut work.search,
            );
        }
    } else if app.extensions.is_some() && !modal_wins(app) {
        on_extensions_key(
            app,
            backend,
            dialog_resolver,
            lang,
            help_lines,
            key.modifiers,
            key.code,
        )
        .await;
    } else if app.key_owner() == crate::app::KeyOwner::Tree && !modal_wins(app) {
        // #136: el árbol manda el listado a la rama
        // elegida por el flujo de cd de siempre.
        let outcome = on_tree_key(
            app,
            backend,
            events,
            dialog_resolver,
            key.modifiers,
            key.code,
        )
        .await;
        apply_cd(
            &app.panes,
            &mut work.fill,
            &mut work.decorate,
            &mut work.probed,
            &mut work.search,
            outcome,
        );
    } else if app.key_owner() == crate::app::KeyOwner::Places && !modal_wins(app) {
        // Sidebar de sitios (L3): Enter sobre una fila
        // manda el LISTADO enfocado a ese sitio, por el
        // flujo de cd de siempre.
        let outcome = on_places_key(
            app,
            backend,
            events,
            dialog_resolver,
            key.modifiers,
            key.code,
        )
        .await;
        settle_cd(
            app,
            backend,
            &mut work.fill,
            &mut work.decorate,
            &mut work.probed,
            &mut work.search,
            outcome,
        );
    } else if app.key_owner() == crate::app::KeyOwner::Processes && !modal_wins(app) {
        // Panel de procesos (#243): las teclas llegan
        // AQUÍ, y no al listado de detrás. Sin este
        // brazo el panel cogía el borde de foco, el
        // `▶` no se movía nunca y F8 abría el diálogo
        // de borrar sobre la selección del listado
        // mientras el lector creía tener el teclado en
        // la lista de tareas.
        on_processes_key(app, dialog_resolver, key.modifiers, key.code);
    } else if app.key_owner() == crate::app::KeyOwner::Log && !modal_wins(app) {
        // Panel de registro (#323), por el mismo motivo que el de procesos:
        // sus teclas son de una letra y tienen que llegar aquí y no al
        // listado, donde `d` es otra cosa.
        crate::logview::apply(app, dialog_resolver, key.modifiers, key.code);
    } else if app.key_owner() == crate::app::KeyOwner::DiskMap && !modal_wins(app) {
        // El mapa de disco (fase 4), por el mismo motivo que sus vecinos: sus
        // teclas son flechas y una letra suelta, y tienen que llegar aquí y no
        // al listado, donde `r` es otra cosa.
        on_disk_map_key(app, dialog_resolver, key.modifiers, key.code);
    } else if app.key_owner() == crate::app::KeyOwner::Panel && !modal_wins(app) {
        // Panel de plugin (fase 3), por el mismo motivo que los dos de
        // arriba: sin este brazo las teclas caían al listado de detrás
        // mientras la pantalla decía que el teclado estaba aquí.
        on_panel_key(app, dialog_resolver, key.modifiers, key.code);
    } else if app.nav_popup.is_some() && !modal_wins(app) {
        // Popup historial/hotlist (spec 2026-07-18): Enter
        // sobre un item NAVEGA por el flujo de cd normal —
        // su desenlace toca el relleno como cualquier cd.
        let outcome = on_nav_popup_key(
            app,
            backend,
            events,
            dialog_resolver,
            key.modifiers,
            key.code,
        )
        .await;
        settle_cd(
            app,
            backend,
            &mut work.fill,
            &mut work.decorate,
            &mut work.probed,
            &mut work.search,
            outcome,
        );
    } else if app.search_dialog.is_some() && !modal_wins(app) {
        // Diálogo Alt+F7 (liveSearch T6): captura imprimibles
        // como los demás overlays; Enter con criterio lanza la
        // búsqueda (abre el pane virtual) — el resto de teclas
        // no navegan.
        if let Some(params) = on_search_dialog_key(app, key.modifiers, key.code) {
            launch_search(app, backend, &mut work.fill, &mut work.search, params).await;
        }
    } else if app.splash.is_some() && !modal_wins(app) {
        // La pantalla de arranque (spec 2026-09-15): cualquier tecla la quita,
        // y con `home` un número abre su fila. Va ANTES que el asistente en la
        // cadena porque solo uno de los dos puede estar puesto —la puerta del
        // splash cede ante él— y así la rama se lee sin pensar en el otro.
        if let Some((cmd, arg)) = crate::splash::on_key(app, key.code) {
            app.pending_splash_row = Some((cmd, arg));
        }
    } else if app.wizard.is_some() && !modal_wins(app) {
        // El asistente de primer arranque (spec 2026-09-10): teclas FIJAS,
        // como la paleta —no hay preset todavía, es justo lo que pregunta—.
        // Lo elegido se escribe al terminar, por el mismo camino que la
        // pantalla de ajustes.
        if let Some(outcome) = crate::wizard::apply_key(app, key) {
            crate::wizard::finish(app, backend, outcome).await;
        }
    } else if app.sync.is_some() && !modal_wins(app) {
        // Panel de sincronización: teclas FIJAS, como las del
        // de diferencias. Va ANTES que él porque se pinta
        // encima: el de diferencias sigue vivo detrás con sus
        // marcas, y el teclado tiene que ir a lo que se ve.
        on_sync_key(app, &mut work.sync, key.modifiers, key.code);
    } else if app.compare.is_some() && !modal_wins(app) {
        // Panel de diferencias (`Shift+F2`): teclas FIJAS,
        // como el diálogo de búsqueda y la palette. No resuelve
        // por el contexto `dialog` porque no hay vocabulario
        // `dialog.*` para «cambia de lado» ni «esconde los
        // iguales», y no es una pantalla del keymap propia
        // porque eso serían siete presets tocados por una
        // tecla que todavía no tiene idioma establecido.
        on_compare_key(
            app,
            backend,
            events,
            &mut work.fill,
            &mut work.decorate,
            &mut work.probed,
            &mut work.search,
            &mut work.compare,
            key.modifiers,
            key.code,
        )
        .await;
    } else if app.goto.is_some() && !modal_wins(app) {
        // «Ir a cualquier sitio» (fase 6): teclas FIJAS, por lo mismo que
        // las de la paleta —no hay verbo `dialog.*` para «teclea una letra»
        // ni para «vete a lo que señalo»— y `ctrl+c` conserva su salida
        // global como en todos los overlays.
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            app.quit = true;
            return;
        }
        let plain = key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT;
        match key.code {
            KeyCode::Char(c) if plain => {
                if let Some(g) = &mut app.goto {
                    g.push_char(c);
                }
                crate::jobs::goto::pedir_al_indice(app, backend, work);
            }
            KeyCode::Backspace if plain => {
                if let Some(g) = &mut app.goto {
                    g.backspace();
                }
                crate::jobs::goto::pedir_al_indice(app, backend, work);
            }
            KeyCode::Esc if plain => {
                app.goto = None;
                crate::jobs::goto::olvidar(work);
            }
            KeyCode::Up if plain => {
                if let Some(g) = &mut app.goto {
                    g.up();
                }
            }
            KeyCode::Down if plain => {
                if let Some(g) = &mut app.goto {
                    g.down();
                }
            }
            KeyCode::Enter if plain => {
                let key = app
                    .goto
                    .as_ref()
                    .and_then(|g| g.selected().map(|r| r.key.clone()));
                // La pantalla se cierra ANTES de actuar, y la petición al
                // índice se abandona: lo que venga detrás —un cd, un
                // comando, un modal— manda en la pantalla, y una respuesta
                // tardía no tiene ya dónde caer.
                app.goto = None;
                crate::jobs::goto::olvidar(work);
                let Some(key) = key else { return };
                match crate::goto::accion(app, &key) {
                    crate::goto::Accion::Ir(dir) => {
                        let outcome = crate::navigate::cd(app, backend, events, dir).await;
                        settle_cd(
                            app,
                            backend,
                            &mut work.fill,
                            &mut work.decorate,
                            &mut work.probed,
                            &mut work.search,
                            outcome,
                        );
                    }
                    // Un comando elegido aquí corre EXACTAMENTE como si se
                    // hubiera pulsado su tecla: el MISMO `run_command` que
                    // invoca el resolver, como ya hace la paleta. Dos
                    // caminos para el mismo verbo divergen en cuanto uno
                    // crece un detalle.
                    //
                    // Sus filas son las de la paleta, que nacen de
                    // `COMMANDS`, así que el parse no puede fallar; el
                    // guard es defensivo, igual que allí.
                    crate::goto::Accion::Comando(cmd) => {
                        let Some(cmd) = Command::parse(&cmd) else {
                            debug_assert!(false, "goto fuera de COMMANDS");
                            return;
                        };
                        run_command(
                            app,
                            backend,
                            events,
                            help_lines,
                            lang,
                            quick_mode,
                            confirm_quit,
                            cfg,
                            &mut work.fill,
                            &mut work.decorate,
                            &mut work.probed,
                            &mut work.search,
                            cmd,
                        )
                        .await;
                        launch_pending(app, events, capture).await;
                    }
                    crate::goto::Accion::Nada(clave) => {
                        app.message = Some(norte_i18n::t(clave));
                    }
                }
            }
            _ => {}
        }
    } else if app.palette.is_some() && !modal_wins(app) {
        // Command palette (H1 T4): editor de filtro libre,
        // como el diálogo de búsqueda de arriba — sus
        // teclas son FIJAS, no resuelven por el contexto
        // `dialog` (decisión 8 del plan H1: no hay
        // vocabulario `dialog.*` para "teclear un carácter"
        // o "correr la selección"). ctrl+c conserva su
        // significado global (salir), como TODOS los
        // overlays.
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            app.quit = true;
            return;
        }
        let plain = key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT;
        match key.code {
            KeyCode::Char(c) if plain => {
                if let Some(p) = &mut app.palette {
                    p.push_char(c);
                }
            }
            KeyCode::Backspace if plain => {
                if let Some(p) = &mut app.palette {
                    p.backspace();
                }
            }
            KeyCode::Esc if plain => app.palette = None,
            // H3c: el puente hacia la página que documenta la
            // fila resaltada. Va AQUÍ, explícito junto a
            // `ctrl+c`/`ctrl+p`, porque las teclas de la
            // palette son FIJAS (decisión 8, arriba): no hay
            // verbo `dialog.*` para «explícame esta fila», así
            // que tampoco puede resolverse por el keymap.
            KeyCode::F(1) if plain => {
                palette_help(app, lang, help_lines);
            }
            KeyCode::Up if plain => {
                if let Some(p) = &mut app.palette {
                    p.up();
                }
            }
            KeyCode::Down if plain => {
                if let Some(p) = &mut app.palette {
                    p.down();
                }
            }
            KeyCode::PageUp if plain => {
                if let Some(p) = &mut app.palette {
                    p.page_up(PAGE);
                }
            }
            KeyCode::PageDown if plain => {
                if let Some(p) = &mut app.palette {
                    p.page_down(PAGE);
                }
            }
            KeyCode::Enter if plain => {
                let cmd = app.palette.as_ref().and_then(Palette::selected);
                app.palette = None;
                if let Some(cmd) = cmd {
                    // Lo lanzado va arriba la próxima vez (spec 2026-09-10);
                    // la sesión lo guarda con el siguiente empuje.
                    norte_frontend::session::note_palette_recent(&mut app.palette_recent, &cmd);
                    // (P1) Enter sobre una fila de PLUGIN: la
                    // `key` es `plugin:{id}:{command}`
                    // (`palette::plugin_rows`, jamás pintada)
                    // — no vive en `COMMANDS`, así que se
                    // enruta AQUÍ, antes del vocabulario
                    // tipado (#112). El resultado del plugin
                    // es texto NO confiable: `detail_for_bar`
                    // (enmascarado + tope, patrón #73).
                    if let Some((id, command)) = parse_plugin_key(&cmd) {
                        let (id, command) = (id.to_owned(), command.to_owned());
                        run_plugin_command(app, backend, &id, &command).await;
                        return;
                    }
                    // Una fila de RENAMER (C3, ADR 0095): pide el plan al
                    // plugin y lo deja en el MISMO run que el de la IA.
                    if let Some((id, renamer)) = norte_frontend::palette::parse_renamer_key(&cmd) {
                        let (id, renamer) = (id.to_owned(), renamer.to_owned());
                        crate::jobs::spawn_renamer_plan(app, backend, work, &id, &renamer);
                        return;
                    }
                    // MISMA función de despacho que el
                    // resolver del keymap invoca (#dispatch):
                    // un comando elegido en la palette corre
                    // EXACTAMENTE como si su tecla se
                    // hubiera pulsado — incluida la apertura
                    // de otro overlay (p.ej. `app.help`).
                    // Las filas de la palette nacen de
                    // `COMMANDS`, así que el parse no puede
                    // fallar; el guard es defensivo (#112).
                    let Some(cmd) = Command::parse(&cmd) else {
                        debug_assert!(false, "palette fuera de COMMANDS");
                        return;
                    };
                    // Paridad con el sitio del resolver (#118
                    // review): un cd elegido en la palette
                    // (nav.parent…) también puede apagar el
                    // modo virtual — cosecha del run (regla 3);
                    // y un `pane.open` de la palette deja su
                    // comando externo resuelto — lanzarlo YA,
                    // no en la siguiente tecla.
                    run_command(
                        app,
                        backend,
                        events,
                        help_lines,
                        lang,
                        quick_mode,
                        confirm_quit,
                        cfg,
                        &mut work.fill,
                        &mut work.decorate,
                        &mut work.probed,
                        &mut work.search,
                        cmd,
                    )
                    .await;
                    launch_pending(app, events, capture).await;
                }
            }
            _ => {}
        }
    } else if app.shortcuts.is_some() && !modal_wins(app) {
        // K3c: el editor de atajos se pinta POR ENCIMA del
        // overlay de ajustes (que sigue abierto detrás), así
        // que también se queda las teclas ANTES que él. En
        // modo captura son TODAS suyas — eso es lo que
        // significa capturar.
        on_shortcuts_key(
            app,
            cfg,
            cli_preset,
            &Maps {
                browse: resolver.effective(),
                viewer: viewer_resolver.effective(),
                dialog: dialog_resolver.effective(),
            },
            key.modifiers,
            key.code,
        )
        .await;
    } else if app.settings.is_some() && !modal_wins(app) {
        // Overlay de ajustes (S3): mismo criterio que la
        // palette de arriba (decisión 8 del plan H1) — sus
        // teclas son fijas, hardcodeadas en `on_settings_key`.
        on_settings_key(
            app,
            &Maps {
                browse: resolver.effective(),
                viewer: viewer_resolver.effective(),
                dialog: dialog_resolver.effective(),
            },
            key.modifiers,
            key.code,
        )
        .await;
    } else if help_owns_keys(app) {
        // H3b: overlay de ayuda. La ruta de teclas vive en
        // `on_help_key` (testeable, como `on_columns_key`);
        // aquí solo queda lo que necesita el run loop, que es
        // DESPACHAR la fila activada. El overlay ya se cerró:
        // el comando actúa sobre los panes de debajo y la
        // ayuda taparía la confirmación que abra.
        match on_help_key(app, dialog_resolver, key.modifiers, key.code) {
            // (H3e) Una fila de PLUGIN sale por el MISMO
            // despacho que el Enter de la palette, no por un
            // camino paralelo: `plugin.run_command` es de
            // donde sale la autorización y la ayuda no la
            // rodea. No toca los panes, así que no arrastra la
            // contabilidad de cd del brazo de abajo.
            Some(HelpDispatch::Plugin(id, command)) => {
                run_plugin_command(app, backend, &id, &command).await;
            }
            None => {}
            Some(HelpDispatch::Command(cmd)) => {
                // MISMO despacho y MISMA contabilidad posterior que
                // el Enter de la palette: una fila de la ayuda es
                // `nav.parent` tanto como lo es una de la palette,
                // así que el camino del cd (relleno paginado,
                // decoración, cosecha de la búsqueda viva, opener
                // externo pendiente) tiene que ser el mismo.
                run_command(
                    app,
                    backend,
                    events,
                    help_lines,
                    lang,
                    quick_mode,
                    confirm_quit,
                    cfg,
                    &mut work.fill,
                    &mut work.decorate,
                    &mut work.probed,
                    &mut work.search,
                    cmd,
                )
                .await;
                launch_pending(app, events, capture).await;
            }
        }
    } else if app.modal.is_some() {
        // MINOR-4 (H1 close): un modal llegado mientras la
        // palette estaba abierta la cierra AQUÍ — obsoleta,
        // y esta MISMA tecla responde al modal en vez de
        // desaparecer dentro del filtro de la palette. El
        // overlay de ajustes (S3) es el MISMO caso: un modal
        // asíncrono (p.ej. una aprobación de policy) gana.
        // El resto de overlays (selector de tema, picker de
        // columnas, extensiones, popup de navegación,
        // diálogo de búsqueda) también ceden la tecla
        // (`modal_wins`) pero NO se cierran: sus filas no
        // caducan como las de la palette/ajustes, y el
        // usuario los recupera intactos al responder. La
        // ayuda es el caso mixto (H3c) y lo decide
        // `close_stale_overlays`.
        close_stale_overlays(app);
        // El TOFU de Lua se resuelve AQUÍ (necesita el host,
        // que vive en este loop): no navega ni toca `work.fill`.
        if matches!(app.modal, Some(Modal::TrustLuaInit { .. })) {
            resolve_lua_trust(app, lua_host, key.code).await;
            return;
        }
        // ctrl+c conserva su significado global (salir),
        // como los demás overlays (H1 T2) — ANTES de
        // resolver contra el contexto `dialog`, hardcodeado.
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            app.quit = true;
            return;
        }
        // #325: el campo de contraseña teclea y borra AQUÍ, pero
        // Enter y Esc siguen por el contexto `dialog` (allowlist
        // `ALLOW_ASK_SECRET`). No entra en la maquinaria de
        // `PromptKind` a propósito: esa presta el campo como
        // `&mut String` —y ofrece un `text()` para pintarlo—, que
        // es justo lo que un secreto no puede dar (regla 10).
        if let Some(Modal::AskSecret { input, .. }) = app.modal.as_mut() {
            let plain = key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT;
            match key.code {
                KeyCode::Char(c) if plain => {
                    input.push(c);
                    return;
                }
                KeyCode::Backspace if plain => {
                    input.pop();
                    return;
                }
                _ => {}
            }
        }
        // Los diez prompts de TEXTO LIBRE comparten
        // teclado: teclear, borrar y Esc son la misma
        // operación sobre el prompt abierto, y consumen
        // la tecla ANTES del contexto `dialog` —ninguno
        // tiene ALLOWLIST de `dialog_action`, y `ctrl+c`
        // ya quedó resuelto arriba—. Lo único propio de
        // cada uno es Enter: ahí vive su submit, que es
        // async y por eso está aquí y no en `App`.
        let prompt = app.modal.as_ref().and_then(Modal::prompt_kind);
        if let Some(kind) = prompt {
            let plain = key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT;
            match key.code {
                KeyCode::Char(c) if plain => app.prompt_push(kind, c),
                KeyCode::Backspace if plain => app.prompt_pop(kind),
                KeyCode::Esc if plain => app.cancel_prompt(kind),
                KeyCode::Enter if plain => match kind {
                    PromptKind::MarkPattern => {
                        if let Ok(n) = app.mark_pattern_confirm() {
                            app.message =
                                Some(ta("msg-marked-by-pattern", &[("n", &n.to_string())]));
                        }
                    }
                    PromptKind::Mkdir => {
                        if let Some(target) = app.mkdir_confirm() {
                            match backend.mkdir(&target).await {
                                Ok(task) => {
                                    app.board.push(&task, None);
                                    app.mkdir_submitted();
                                }
                                // MINOR-1: el nombre sobrevive
                                // al fallo del submit.
                                Err(e) => app.mkdir_set_error(error_message(&e)),
                            }
                        }
                    }
                    // #290: crear el fichero es una task del daemon como
                    // cualquier otra mutación; el editor se abre en el tick
                    // que la ve terminar, no aquí.
                    PromptKind::EditNew => {
                        if let Some(target) = app.edit_new_confirm() {
                            match backend.create_file(&target).await {
                                Ok(task) => {
                                    app.pending_edit_open = Some((task.id(), target));
                                    app.board.push(&task, None);
                                    app.edit_new_submitted();
                                }
                                Err(e) => app.edit_new_set_error(error_message(&e)),
                            }
                        }
                    }
                    // #306: guardar el espacio de trabajo como perfil. Escribe
                    // TRES ficheros con lock y tmp+rename, así que va por
                    // `spawn_blocking` (regla 2) y el modal se cierra solo
                    // cuando el disco contestó que sí.
                    PromptKind::ProfileSaveAs => {
                        crate::screens::profile_save_as(app).await;
                    }
                    PromptKind::Pack => {
                        if let Some(params) = app.pack_confirm() {
                            match backend.pack(params).await {
                                Ok(task) => {
                                    app.board.push(&task, None);
                                    app.pack_submitted();
                                }
                                Err(e) => {
                                    app.pack_set_error(error_message(&e));
                                }
                            }
                        }
                    }
                    PromptKind::Split => {
                        if let Some(params) = app.split_confirm() {
                            match backend.split_file(params).await {
                                Ok(task) => {
                                    app.board.push(&task, None);
                                    app.split_submitted();
                                }
                                Err(e) => {
                                    app.split_set_error(error_message(&e));
                                }
                            }
                        }
                    }
                    // #314: los permisos. Mismo molde que partir: se resuelven
                    // los params, se manda, y un fallo del submit CONSERVA lo
                    // tecleado con su diagnóstico debajo.
                    PromptKind::Chmod => {
                        if let Some(params) = app.chmod_confirm() {
                            let n = params.paths.len();
                            match backend.set_mode(params).await {
                                Ok(task) => {
                                    app.board.push(&task, None);
                                    app.chmod_submitted();
                                    app.message =
                                        Some(ta("msg-chmod-started", &[("n", &n.to_string())]));
                                }
                                Err(e) => app.chmod_set_error(error_message(&e)),
                            }
                        }
                    }
                    PromptKind::TransferDest => {
                        let _ = app.transfer_dest_confirm();
                    }
                    PromptKind::CommandLine => {
                        if let Some(cmd) = app.command_line_confirm() {
                            submit_command_line(app, &cmd);
                        }
                    }
                    PromptKind::AiRename => {
                        if let Some(instruction) = app.ai_rename_confirm() {
                            let dir = app.focused().dir().clone();
                            let b = backend.clone();
                            let d = dir.clone();
                            // Lo MARCADO, si hay marcas (#121): pedir un plan
                            // sobre cinco ficheros mandaba los mil del
                            // directorio al proveedor, que es más de lo que el
                            // humano señaló. `marked_entries` y no
                            // `marked_paths`: el segundo cae al cursor cuando
                            // no hay marcas, y «sin marcar nada» significa el
                            // directorio entero.
                            let cuantas_marcas = app.focused().marked_entries().len();
                            let marcados: Vec<String> = app
                                .focused()
                                .marked_entries()
                                .iter()
                                .filter_map(|e| e.path.file_name())
                                .filter_map(|s| String::from_utf8(s.as_bytes().to_vec()).ok())
                                .collect();
                            // Si TODO lo marcado son nombres que no son texto,
                            // la lista queda vacía — y vacía significa «el
                            // directorio entero», o sea justo lo contrario de
                            // lo que se pidió. Se rehúsa y se dice: ampliar el
                            // alcance en silencio es lo que este campo existe
                            // para no hacer.
                            if cuantas_marcas > 0 && marcados.is_empty() {
                                app.message = Some(t("msg-ai-rename-marks-not-text"));
                                app.ai_rename_submitted();
                                return;
                            }
                            let handle = tokio::spawn(async move {
                                b.ai_rename_plan(&d, &instruction, &marcados).await
                            });
                            // Relanzar con un run vivo lo ABORTA
                            // (dropear el handle solo desvincula):
                            // a lo sumo una petición en vuelo.
                            let names: Vec<Vec<u8>> = app
                                .focused()
                                .entries()
                                .iter()
                                .filter_map(|e| e.path.file_name().map(|s| s.as_bytes().to_vec()))
                                .collect();
                            let run = AiRenameRun { handle, dir, names };
                            if let Some(old) = work.ai_rename.replace(run) {
                                old.handle.abort();
                            }
                            // Invariante: lanzar VACÍA el stash —
                            // un plan retenido de una petición
                            // ANTERIOR jamás debe abrirse como si
                            // fuera de esta.
                            work.pending_ai_plan = None;
                            app.message = Some(t("msg-ai-rename-running"));
                            app.ai_rename_submitted();
                        }
                    }
                    // #310: la plantilla ya validada produce los pares AQUÍ
                    // —sin salir a preguntarle a nadie— y a partir de ahí el
                    // camino es el MISMO que el del plan de la IA: se pide
                    // `fs.rename_batch_plan`, se abre el modal en `Pending` y
                    // el harvest lo rellena.
                    PromptKind::RenameBatch => {
                        if let Some(pattern) = app.rename_batch_confirm() {
                            let names = app.rename_batch_names();
                            let dir = app.focused().dir().clone();
                            let entries: Vec<norte_proto::methods::AiRenameEntry> =
                                norte_frontend::rename_pattern::plan(&pattern, &names, 1)
                                    .into_iter()
                                    .map(|(from, to)| norte_proto::methods::AiRenameEntry {
                                        from,
                                        to,
                                    })
                                    .collect();
                            app.rename_batch_submitted();
                            if entries.is_empty() {
                                // Una plantilla que deja todo igual no es un
                                // error: no hay nada que renombrar y se dice.
                                app.message = Some(t("msg-rename-batch-no-changes"));
                            } else if let Some(pairs) = norte_frontend::rename_pairs(&entries) {
                                let b = backend.clone();
                                let d = dir.clone();
                                let handle =
                                    tokio::spawn(
                                        async move { b.rename_batch_plan(&d, &pairs).await },
                                    );
                                if let Some(old) =
                                    work.rename_batch.replace(RenameBatchRun { handle })
                                {
                                    old.handle.abort();
                                }
                                app.modal = Some(Modal::AiRenamePlan {
                                    dir,
                                    entries,
                                    offset: 0,
                                    // La primera ventana ya se ha visto en
                                    // cuanto el modal abre.
                                    seen: norte_frontend::AI_RENAME_PAIR_LIMIT,
                                    plan: norte_frontend::BatchPlan::Pending,
                                });
                            } else {
                                // Un par que no es un `Segment` es un bug
                                // NUESTRO —la plantilla los fabricó— y no una
                                // respuesta hostil: se dice y no se pide plan.
                                app.message = Some(t("msg-rename-pattern-bad-result"));
                            }
                        }
                    }
                    PromptKind::Semantic => {
                        if let Some(query) = app.semantic_confirm() {
                            let b = backend.clone();
                            let handle = tokio::spawn(async move {
                                b.index_search_semantic(None, &query, SEMANTIC_K).await
                            });
                            // Relanzar con un run vivo lo ABORTA
                            // (dropear el handle solo desvincula):
                            // a lo sumo una consulta en vuelo.
                            if let Some(old) = work.semantic.replace(SemanticRun { handle }) {
                                old.handle.abort();
                            }
                            // Invariante: lanzar VACÍA el stash —
                            // unos hits retenidos de una consulta
                            // ANTERIOR jamás deben abrirse como si
                            // fueran de esta.
                            work.pending_semantic = None;
                            app.message = Some(t("msg-semantic-running"));
                            app.semantic_submitted();
                        }
                    }
                    PromptKind::TransferName => {
                        if let Some((kind, from, dest)) = app.transfer_name_confirm() {
                            // Cierra SOLO si encoló (disciplina
                            // MINOR-1 de #104): un submit
                            // fallido conserva el nombre; el
                            // detalle queda en la barra.
                            if submit_transfer(
                                app,
                                backend,
                                kind,
                                from,
                                dest,
                                TransferOptions::default(),
                            )
                            .await
                            {
                                app.transfer_name_submitted();
                            } else {
                                app.transfer_name_set_error(t("msg-transfer-name-failed"));
                            }
                        }
                    }
                },
                _ => {}
            }
            return;
        }
        // El modal TOFU (#45) puede NAVEGAR al confiar: su Cd
        // se aplica igual que el de un comando.
        let outcome = on_dialog_key(
            app,
            backend,
            events,
            dialog_resolver,
            key.modifiers,
            key.code,
            lang,
            help_lines,
        )
        .await;
        settle_cd(
            app,
            backend,
            &mut work.fill,
            &mut work.decorate,
            &mut work.probed,
            &mut work.search,
            outcome,
        );
    } else {
        // Esc con un comando Lua en vuelo (BROWSE: sin modal
        // ni overlay, y NO en el viewer): pide cancelación
        // (regla 3) y CONSUME la tecla — no cae al resolver.
        if app.viewer.is_none()
            && key.modifiers.is_empty()
            && key.code == KeyCode::Esc
            && let Some((_, token)) = &work.lua
        {
            token.cancel();
            // K3a: la tecla se CONSUME aquí, así que el
            // resolver no la ve — y una secuencia a medias
            // (con su panel which-key encima) se quedaría
            // armada mientras el lector cree haber cancelado.
            app.abandon_pending(resolver);
            return;
        }
        // Esc con ai.rename_plan en vuelo (BROWSE, M4-IA):
        // cancelar (regla 3) y CONSUMIR la tecla. Abortar
        // dropea el future del backend en el runtime →
        // rpc.cancel (remoto) / drop del stream (embebido).
        if app.viewer.is_none()
            && key.modifiers.is_empty()
            && key.code == KeyCode::Esc
            && let Some(run) = work.ai_rename.take()
        {
            run.handle.abort();
            app.message = None;
            // K3a: ídem — Esc consumido aquí también cancela
            // la secuencia en vuelo, jamás solo su pintura.
            app.abandon_pending(resolver);
            return;
        }
        // Esc con una búsqueda semántica en vuelo (BROWSE,
        // M4-IA-2): mismo contrato de cancelación (regla 3).
        if app.viewer.is_none()
            && key.modifiers.is_empty()
            && key.code == KeyCode::Esc
            && let Some(run) = work.semantic.take()
        {
            run.handle.abort();
            app.message = None;
            app.abandon_pending(resolver);
            return;
        }
        // Pane virtual de búsqueda (liveSearch T6): con un
        // work.search en el pane con foco (y sin quick vivo),
        // Esc y Enter tienen semántica propia ANTES del
        // resolver. El RESTO de teclas (cursor, F5/F8/F3…) cae
        // al resolver y opera sobre el hit bajo el cursor.
        if app.viewer.is_none()
            && key.modifiers.is_empty()
            && app.focused().quick().is_none()
            && app.focused().virtual_search
            && work.search.as_ref().is_some_and(|s| s.pane == app.focus())
        {
            match key.code {
                KeyCode::Esc => {
                    // Task viva → cancela (hits conservados,
                    // pasará a Cancelled al cerrarse el canal).
                    // Ya terminada → sale del modo virtual
                    // restaurando el dir anterior.
                    on_search_escape(
                        app,
                        backend,
                        events,
                        &mut work.fill,
                        &mut work.decorate,
                        &mut work.probed,
                        &mut work.search,
                    )
                    .await;
                    return;
                }
                KeyCode::Enter => {
                    // Enter sobre un hit: cd al PADRE del hit y
                    // cursor sobre él (sale del modo virtual).
                    on_search_enter(
                        app,
                        backend,
                        events,
                        &mut work.fill,
                        &mut work.decorate,
                        &mut work.probed,
                        &mut work.search,
                    )
                    .await;
                    return;
                }
                _ => {}
            }
        }
        // Quick search ACTIVO en el pane con foco (BROWSE):
        // sus teclas se comen ANTES del resolver — un char
        // (incluida otra `/`) alimenta la query y jamás
        // re-entra al keymap (sin recursión). El RESTO de
        // teclas (F5, F8, F3, Tab en Filter…) NO se consume:
        // cae al resolver y opera sobre `selected()` ya
        // filtrado — feed-to-listbox gratis. Decisión
        // consciente: las teclas de navegación NO
        // interceptadas (PageUp/PageDown/Home/End) también
        // caen al resolver y mueven el CURSOR REAL, que con
        // el filtro activo es invisible; al cancelar (Esc)
        // reaparece donde lo dejaron. Conectarlas a la
        // selección del filtro no compensa el estado extra.
        if app.viewer.is_none() && app.focused().quick().is_some() {
            let jump = app
                .focused()
                .quick()
                .is_some_and(|q| q.mode() == nav::Mode::Jump);
            // SHIFT pasa (una mayúscula llega como
            // Char('A')+SHIFT y el char ya viene tal cual);
            // ctrl/alt caen al resolver (ctrl+c sigue
            // saliendo).
            let plain = key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT;
            match key.code {
                KeyCode::Char(c) if plain => {
                    app.focused_mut().quick_char(c);
                    return;
                }
                KeyCode::Backspace if plain => {
                    app.focused_mut().quick_backspace();
                    return;
                }
                KeyCode::Up if plain => {
                    app.focused_mut().quick_up();
                    return;
                }
                KeyCode::Down if plain => {
                    app.focused_mut().quick_down();
                    return;
                }
                KeyCode::Tab if plain && jump => {
                    app.focused_mut().quick_next();
                    return;
                }
                KeyCode::Esc if plain => {
                    app.focused_mut().quick_cancel();
                    return;
                }
                KeyCode::Enter if plain => {
                    // Confirma (cursor real = seleccionado) y
                    // REUSA el camino de nav.enter: un dir (o
                    // contenedor) entra, un fichero se queda.
                    // `false` = el filtro no tenía matches:
                    // solo cierra — jamás despachar sobre una
                    // entrada que el usuario no veía (review
                    // MAJOR T4).
                    if app.focused_mut().quick_confirm() {
                        // Paridad con el sitio del resolver
                        // (#118 review): el Enter del quick
                        // search ES un nav.enter — entrar en
                        // un hit apaga el modo virtual del
                        // pane; sin cosecha, la Task de
                        // búsqueda quedaba viva (regla 3).
                        run_command(
                            app,
                            backend,
                            events,
                            help_lines,
                            lang,
                            quick_mode,
                            confirm_quit,
                            cfg,
                            &mut work.fill,
                            &mut work.decorate,
                            &mut work.probed,
                            &mut work.search,
                            Command::NavEnter,
                        )
                        .await;
                    }
                    return;
                }
                _ => {}
            }
        }
        // `--pick` (S2, design §B): Enter/Ctrl+Enter accept
        // the selection HERE, where the key turns into a
        // command — never in a preset, which must not have
        // to know `--pick` exists. Browse only (the viewer
        // has nothing to pick); quick search and the
        // live-search pane already resolved their own Enter
        // above and `continue`d past this point, so reaching
        // here means neither is active.
        if app.pick && app.viewer.is_none() {
            let ctrl_enter = key.code == KeyCode::Enter && key.modifiers == KeyModifiers::CONTROL;
            // Plain Enter keeps navigating whenever there is
            // somewhere to go (`nav_enter_target`) — taking
            // that away would make the picker unusable for
            // reaching anything below the start directory.
            let plain_enter = key.code == KeyCode::Enter
                && key.modifiers.is_empty()
                && nav_enter_target(app).is_none();
            if ctrl_enter || plain_enter {
                app.abandon_pending(resolver);
                let _ = dispatch(
                    app,
                    backend,
                    events,
                    help_lines,
                    lang,
                    quick_mode,
                    confirm_quit,
                    cfg,
                    Command::AppPickAccept,
                )
                .await;
                return;
            }
        }
        // Pantalla activa: el viewer tiene su contexto.
        let active = if app.viewer.is_some() || app.key_owner() == crate::app::KeyOwner::Preview {
            &mut *viewer_resolver
        } else {
            &mut *resolver
        };
        // Teclas que el keymap no modela (Media, BackTab,
        // CapsLock…) no llegan al resolver como chord, pero
        // el trato SÍ es el mismo que un `Resolution::Reset`:
        // `active.reset()` rompe cualquier secuencia
        // pendiente EN EL RESOLVER (no solo el `app.pending`
        // de pantalla) — antes `from_event` siempre empujaba
        // un chord (aunque exótico) y el `Miss` resultante
        // limpiaba el pending interno; `chord_from_crossterm`
        // devuelve `None` en su lugar, así que el reset hay
        // que pedirlo explícito, jamás dejar la secuencia a
        // medias viva.
        if let Some(chord) = chord_from_crossterm(key.modifiers, key.code) {
            match active.push(chord) {
                Resolution::Run {
                    command: cmd,
                    count,
                } => {
                    // K3a: cierra TAMBIÉN el panel which-key, y
                    // antes de `keyboard_owner(app)` — el
                    // fingerprint del contador se toma con el
                    // panel ya cerrado, así que el cierre no
                    // cuenta como «el despacho movió el
                    // teclado» y no parte un `5j`.
                    app.clear_pending();
                    // K2a: un contador sobre un comando que no
                    // lo acepta NO se traga — corre una vez y
                    // se dice. Se pone ANTES del despacho a
                    // propósito: si el comando tiene algo que
                    // decir, su mensaje es el que manda.
                    if let Count::Ignored(n) = count {
                        app.message = Some(count_ignored_message(&cmd, n));
                    }
                    // `lua:<nombre>` (M4): al despachador Lua —
                    // jamás a `dispatch` (no es comando fijo).
                    // Un `lua:` no está en el catálogo, así que
                    // su contador siempre es `Ignored`: corre
                    // UNA vez, sin bucle.
                    if let Some(name) = cmd.strip_prefix("lua:") {
                        run_lua_command(
                            app,
                            lua_host,
                            backend,
                            name,
                            &mut work.lua,
                            &mut work.lua_queue,
                        );
                        return;
                    }
                    // #112: el keymap se validó contra
                    // COMMANDS al cargar — el parse no puede
                    // fallar; guard defensivo.
                    let Some(cmd) = Command::parse(&cmd) else {
                        debug_assert!(false, "keymap fuera de COMMANDS");
                        return;
                    };
                    // El contador repite el DESPACHO: ninguna
                    // firma de comando cambia y ninguno puede
                    // olvidarse de honrarlo. El cuerpo entero
                    // (outcome, cd, cosecha, opener) va DENTRO
                    // — un `dispatch` sin su outcome deja
                    // Tasks vivas y panes sin refrescar.
                    // Ningún `continue` del loop exterior vive
                    // aquí dentro: los dos que tenía este
                    // brazo (la rama Lua y el guard del parse)
                    // quedan ARRIBA, antes del bucle, así que
                    // el contador no puede saltarse.
                    let owner_before = keyboard_owner(app);
                    for _ in 0..count.times() {
                        let outcome = dispatch(
                            app,
                            backend,
                            events,
                            help_lines,
                            lang,
                            quick_mode,
                            confirm_quit,
                            cfg,
                            cmd,
                        )
                        .await;
                        // Leído ANTES de que `apply_cd`
                        // consuma el outcome.
                        let stalled = nav_stalled(cmd, &outcome);
                        settle_cd(
                            app,
                            backend,
                            &mut work.fill,
                            &mut work.decorate,
                            &mut work.probed,
                            &mut work.search,
                            outcome,
                        );
                        // Un cd (nav.parent…) apagó el modo
                        // virtual del pane de búsqueda: suelta
                        // el run y cancela.
                        reap_search_run(app, &mut work.search);
                        // #28: `pane.open` dejó un comando
                        // externo resuelto — el run loop (dueño
                        // de la terminal) sondea el binario y
                        // lo lanza.
                        launch_pending(app, events, capture).await;
                        // Parar en seco si la app se va:
                        // `9999` seguido de una tecla de salida
                        // no puede encolar 9998 salidas más. El
                        // loop exterior comprueba `app.quit`
                        // tras el draw, así que sin este break
                        // el resto de las vueltas correría con
                        // la app muerta. Lo mismo si el
                        // despacho movió el teclado a otra
                        // superficie (modal, visor, overlay):
                        // lo que quede del contador dispararía
                        // comandos DETRÁS de ella
                        // (`keyboard_owner` los cubre todos, no
                        // solo el modal). Y lo mismo si un paso
                        // del rastro no aterrizó: se rebobina,
                        // así que la vuelta siguiente repetiría
                        // el MISMO listado remoto.
                        if app.quit || stalled || keyboard_owner(app) != owner_before {
                            break;
                        }
                    }
                }
                // K2a: una secuencia a medias y un contador a
                // medio teclear se pintan IGUAL y a la vez —
                // `pending_display` compone los dos (en `12gg`
                // conviven). Un contador que no se ve es un
                // contador que no se puede cancelar.
                //
                // K3a: y el mismo estado abre (o no) el panel
                // which-key. Los dos brazos llaman a UNA sola
                // función porque la barra y el panel describen
                // el MISMO resolver: es `show_pending` quien
                // sabe que un contador suelto no tiene panel
                // (su secuencia pendiente está vacía), no este
                // `match`. Sin temporizador de ningún tipo: el
                // panel aparece con la tecla que deja el
                // prefijo pendiente (ADR 0006).
                Resolution::Pending(_) | Resolution::Counting(_) => {
                    app.show_pending(active, lang);
                }
                // K1 T4: la tecla ESTÁ ligada y esta build no
                // puede correr lo que tiene ligado. Antes se
                // despachaba un nombre sin brazo; ahora la
                // barra de estado dice por qué.
                Resolution::Unavailable { command, why } => {
                    app.clear_pending();
                    app.message = Some(unavailable_message(&command, why));
                }
                Resolution::Reset => app.clear_pending(),
            }
        } else {
            active.reset();
            app.clear_pending();
        }
    }
}
