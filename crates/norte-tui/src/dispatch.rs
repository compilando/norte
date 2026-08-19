//! La tabla de despacho: un nombre de comando (ADR 0006, los mismos que ven
//! la palette y el wire) y su efecto.
//!
//! Vivía en el root del binario `ntc` —un crate DISTINTO de esta lib—, y sale
//! ENTERA. Es un `match` plano de 109 brazos y no se trocea: este repositorio
//! ya argumentó por escrito contra partir tablas planas
//! (`norte-core/src/daemon/server.rs`), y el argumento es el mismo aquí —
//! trocear un match exhaustivo por temas cambia una tabla que el compilador
//! comprueba de una vez por varias que hay que mantener en sintonía a mano.
//!
//! Lo que la hace grande es el vocabulario, no la profundidad: casi todos los
//! brazos son de una a tres líneas, y lo que cada uno llama vive ya en su
//! propio módulo — es lo que las nueve rondas anteriores de esta rama fueron
//! sacando de aquí debajo.

use crate::app::{
    App, ExtensionManager, Modal, NavPopupKind, Palette, Settings, TrailStep, TransferKind,
    error_message,
};
use crate::config;
use crate::gestures::{
    disconnect, edit_under_cursor, mirror_plan, pull_plan, resolve_opener, run_pane_gesture,
    shell_cwd,
};
use crate::keymap::Command;
use crate::mutations::{combine_pieces, launch_size_count, test_archive, unpack};
use crate::nav;
use crate::navigate::{Cd, cd};
use crate::overlays::open_contextual_help;
use crate::refresh::refresh_panes;
use crate::screens::{
    open_drive_popup, plugin_config_summaries, refresh_places_drives, refresh_places_favorites,
};
use crate::trail::{nav_enter_target, walk_trail};
use crate::viewer_open::{open_viewer, viewer_do};
use crossterm::event::EventStream;
use norte_core::backend::Backend;
use norte_i18n::{t, ta};
use norte_proto::EntryKind;

/// Ejecuta un comando nombrado (ADR 0006: los mismos nombres que verán la
/// palette y el wire). Un error de listado en un cd NO tumba el TUI: el
/// pane se queda donde estaba (aviso visible: barra de mensajes, issue #20).
#[allow(clippy::too_many_lines, clippy::too_many_arguments)] // tabla de despacho comando→efecto, no API
pub async fn dispatch(
    app: &mut App,
    backend: &Backend,
    events: &mut EventStream,
    help_lines: &[ratatui::text::Line<'static>],
    // H3b: the negotiated language `Command::AppHelp` opens the corpus in.
    // See the same parameter on `run`.
    lang: norte_i18n::Lang,
    quick_mode: nav::Mode,
    confirm_quit: config::ConfirmQuit,
    // S3 (`app.settings`): la config VIGENTE — solo leída, para construir
    // las filas del overlay al abrirlo (`crate::settings::build_rows`).
    cfg: &config::LoadedConfig,
    cmd: Command,
) -> Cd {
    // Solo los cd (nav.enter/nav.parent) tocan el relleno en background; el
    // resto de comandos lo dejan como está (`Cancelled`).
    let mut cd_outcome = Cd::Cancelled;
    match cmd {
        // S2 (`[ui] confirm_quit`): SOLO este brazo (el despacho nombrado de
        // `app.quit`, alcanzable por keymap Y por la palette) honra la
        // config y puede abrir `Modal::ConfirmQuit`. Los `app.quit = true`
        // hardcodeados de Ctrl+C repartidos por el resto de este fichero
        // (cada overlay tiene el suyo, documentado in situ) son la salida de
        // emergencia — se quedan INMEDIATOS a propósito, jamás preguntan.
        Command::AppQuit => {
            if crate::app::quit_needs_confirm(confirm_quit, app.board.has_active()) {
                app.modal = Some(Modal::ConfirmQuit);
            } else {
                app.quit = true;
            }
        }
        // `--pick` (S2): the run loop is the only caller (the Enter/
        // Ctrl+Enter override below), and only ever under `--pick` — but the
        // guard stays here too, not just there, because `app.pick-accept` is
        // also reachable through the palette (H1 T4 put every catalogue name
        // there) and a preset a user hand-writes could bind it directly.
        // Outside `--pick` this is a no-op: there is nothing to accept into.
        Command::AppPickAccept => {
            if app.pick {
                app.picked = Some(app.focused().marked_paths());
                app.quit = true;
            }
        }
        Command::PaneSwitch => app.switch_focus(),
        Command::TabNew => app.tab_new(),
        Command::TabClose => app.tab_close(),
        Command::TabNext => app.tab_cycle(1),
        Command::TabPrev => app.tab_cycle(-1),
        Command::TabMoveLeft => app.tab_move(-1),
        Command::TabMoveRight => app.tab_move(1),
        Command::TabGoto1 => app.tab_goto(1),
        Command::TabGoto2 => app.tab_goto(2),
        Command::TabGoto3 => app.tab_goto(3),
        Command::TabGoto4 => app.tab_goto(4),
        Command::TabGoto5 => app.tab_goto(5),
        Command::TabGoto6 => app.tab_goto(6),
        Command::TabGoto7 => app.tab_goto(7),
        Command::TabGoto8 => app.tab_goto(8),
        Command::TabGoto9 => app.tab_goto(9),
        Command::AppMenu => {
            // Alternar: la misma tecla lo abre y lo cierra, como los demás
            // overlays.
            app.menu = if app.menu.is_some() {
                None
            } else {
                Some(norte_frontend::menu::MenuState::new())
            };
        }
        Command::LayoutSplitH => app.layout_split(norte_frontend::layout::Dir::Horizontal),
        Command::LayoutSplitV => app.layout_split(norte_frontend::layout::Dir::Vertical),
        Command::LayoutFocusNext => app.layout_focus(1),
        Command::LayoutFocusPrev => app.layout_focus(-1),
        Command::LayoutCloseSlot => {
            if !app.layout_close_slot() {
                app.message = Some(norte_i18n::t("msg-layout-last-panel"));
            }
        }
        Command::LayoutGrow => app.layout_resize(1),
        Command::LayoutShrink => app.layout_resize(-1),
        Command::LayoutEqualize => app.layout_equalize(),
        Command::LayoutSetTarget => app.layout_set_target(),
        // L3: abrir el sidebar es el momento de pedir los volúmenes, y el
        // ÚNICO junto con desplegar su sección. Si ya estaba abierto no se
        // vuelven a pedir: esa pulsación solo se lleva el teclado.
        Command::LayoutPlaces => {
            let was = app.places_slot().is_some();
            app.toggle_places();
            if !was && app.places_drives_visible() {
                refresh_places_drives(app, backend).await;
            }
            refresh_places_favorites(app);
        }
        // El visor acoplado no pide nada aquí: lo que lea sale de
        // `preview::want` en el bucle, contra el cursor de cada frame.
        Command::LayoutPreview => app.toggle_preview(),
        Command::LayoutProcesses => app.toggle_processes(),
        // #136: el árbol se abre, se enfoca y se cierra como el sidebar. Su
        // contenido lo pide el run loop, una rama por vuelta.
        Command::PaneTree => app.toggle_tree(),
        Command::LayoutMetadata => app.toggle_metadata(),
        // El listado del directorio de layouts se hace AQUÍ, fuera del
        // runtime, y llega hecho al `App` (regla 2). Un directorio que no se
        // puede leer da lista vacía: quedan las cinco de fábrica, que es más
        // que nada.
        Command::LayoutPick => {
            // Listar Y LEER, las dos cosas fuera del runtime: cada fila del
            // selector pinta su pantalla, y leerla al pasar el cursor sería
            // I/O en el bucle (#244 M2, regla 2). Sin directorio de config no
            // hay ficheros de usuario: quedan las cinco de fábrica. Antes se
            // caía a `PathBuf::default()`, que es leer `./layouts/` del
            // directorio actual — o sea, clonar un repo y pulsar F9 (#244 m3).
            let mine = match config::user_config_dir() {
                Some(dir) => tokio::task::spawn_blocking(move || {
                    use norte_frontend::layout::config;
                    config::list(&dir)
                        .into_iter()
                        .map(|name| norte_frontend::layout_picker::UserLayout {
                            tree: config::load(&dir, &name).map_err(|e| e.to_string()),
                            name,
                        })
                        .collect::<Vec<_>>()
                })
                .await
                .unwrap_or_default(),
                None => Vec::new(),
            };
            app.open_layout_picker(mine);
        }
        // `pane.mirror`: la ubicación sale del pane con FOCO y viaja el otro.
        Command::PaneMirror => {
            let plan = mirror_plan(app);
            let origin = app.focus();
            cd_outcome = run_pane_gesture(app, backend, events, plan, origin).await;
        }
        // `pane.pull`: el mismo gesto al revés — la ubicación sale del OTRO
        // pane y viaja el del foco.
        Command::PanePull => {
            let plan = pull_plan(app);
            let origin = app.focus() ^ 1;
            cd_outcome = run_pane_gesture(app, backend, events, plan, origin).await;
        }
        // `pane.swap`: NO toca disco — los dos listados ya existían y solo
        // cambian de lado. La mitad que `dispatch` no ve (fill en vuelo,
        // fetches de decoración, dedup de la sonda) viaja al run loop.
        Command::PaneSwap => {
            app.swap_panes();
            cd_outcome = Cd::Swapped;
        }
        Command::NavBack => {
            cd_outcome = walk_trail(app, backend, events, TrailStep::Back).await;
        }
        Command::NavForward => {
            cd_outcome = walk_trail(app, backend, events, TrailStep::Forward).await;
        }
        // `/` (spec 2026-07-18): arranca el quick search en el modo de la
        // config. Con uno ya activo las teclas se comen antes del resolver,
        // así que este brazo solo corre para ABRIRLO — sin recursión.
        Command::PaneQuickSearch => app.focused_mut().quick_start(quick_mode),
        // `Alt+↓` / `Ctrl+D` (spec 2026-07-18): con el popup abierto sus
        // teclas se comen antes del resolver (patrón overlay) — estos
        // brazos solo corren para ABRIRLO.
        Command::PaneHistory => app.open_nav_popup(NavPopupKind::History),
        Command::PaneHotlist => app.open_nav_popup(NavPopupKind::Hotlist),
        // `pane.select-drive*` (design §D): `-left`/`-right` name a SIDE —
        // `panes[0]`/`panes[1]` — not the focus, which is what Total
        // Commander's `Alt+F1`/`Alt+F2` do; only the unsided variant reads
        // `app.focus()`.
        Command::PaneSelectDrive => {
            open_drive_popup(app, backend, app.focus(), false).await;
        }
        Command::PaneSelectDriveLeft => {
            open_drive_popup(app, backend, 0, false).await;
        }
        Command::PaneSelectDriveRight => {
            open_drive_popup(app, backend, 1, false).await;
        }
        // `Alt+F7` (liveSearch T6): abre el diálogo de búsqueda viva. Con él
        // abierto sus teclas se comen antes del resolver (patrón overlay) —
        // este brazo solo corre para ABRIRLO.
        Command::PaneSearch => app.open_search_dialog(),
        // `Shift+F2`: compara los dos panes. Solo RESUELVE los params y los
        // deja en `pending_compare` — lanzar es del run loop, que es quien
        // tiene el canal y la Task.
        Command::PaneCompareDirs => app.request_compare(),
        // `Ctrl+Y`: planifica una sincronización de este pane al otro. Solo
        // RESUELVE los params (y las negativas, la del journal la primera);
        // lanzar es del run loop. `Mirror` no tiene tecla global a propósito:
        // borrar en el destino es lo que se pide desde el panel de
        // diferencias, con lo que se va a borrar delante.
        Command::PaneSyncDirs => {
            app.request_sync(norte_proto::methods::SyncMode::Update);
        }
        Command::CursorUp => app.focused_mut().move_up(1),
        Command::CursorDown => app.focused_mut().move_down(1),
        // #124: una PÁGINA es una pantalla del pane (menos una fila de
        // contexto), no una constante — el alto real llega del último frame.
        Command::CursorPageUp => {
            let step = app.focused().page_step();
            app.focused_mut().move_up(step);
        }
        Command::CursorPageDown => {
            let step = app.focused().page_step();
            app.focused_mut().move_down(step);
        }
        Command::CursorTop => app.focused_mut().move_to_start(),
        Command::CursorBottom => app.focused_mut().move_to_end(),
        Command::NavEnter => {
            if let Some(dir) = nav_enter_target(app) {
                cd_outcome = cd(app, backend, events, dir).await;
            }
        }
        Command::NavParent => {
            // Salir de la raíz interior de un archivo = el dir que CONTIENE
            // al contenedor (el padre sintáctico sería un compuesto sin
            // marcador: malformado, ADR 0018).
            let dir = app.focused().dir().clone();
            // Foco pendiente (spec 2026-07-24 §S1): el hijo del que
            // venimos, para seleccionarlo en el listado del padre. Al salir
            // de la raíz interior de un archivo el hijo NO es `dir` (ese es
            // el path compuesto virtual, no una entrada real del listado
            // del padre) sino el archivo contenedor mismo (`aref.outer`).
            let (parent, child) = match dir.archive_split() {
                Ok(Some(aref)) if aref.inner.is_empty() => {
                    let outer = aref.outer.clone();
                    (outer.parent(), outer)
                }
                _ => (dir.parent(), dir.clone()),
            };
            if let Some(parent) = parent {
                app.focused_mut().set_pending_focus(child);
                cd_outcome = cd(app, backend, events, parent).await;
                // Revisión S, M2: un `cd` FALLIDO (permiso denegado, error
                // del daemon…) nunca llama a `set_listing` (`cd`'s doc, `Err`
                // arm), así que el hint recién fijado arriba nunca se
                // consume — descartarlo aquí evita que sobreviva a un `cd`
                // futuro sin relación. `Cd::Suspended` (el modal TOFU, que
                // REINTENTA esta misma navegación) lo CONSERVA a propósito: el
                // reintento debe seguir aterrizando en `child`. Un
                // `Cd::Cancelled` (Esc) también lo conserva — el lector sigue
                // en el mismo listado, y el hint muere con el siguiente cd que
                // sí aterrice.
                if matches!(cd_outcome, Cd::Failed(_)) {
                    app.focused_mut().clear_pending_focus();
                }
            } else {
                // Raíz `/` o raíz de unidad Windows (`parent()` = None): antes
                // era un no-op SILENCIOSO (#20). Ahora avisa por la barra.
                app.message = Some(t("msg-nav-at-top"));
            }
        }
        Command::PaneCopy | Command::PaneMove => {
            let kind = if cmd == Command::PaneCopy {
                TransferKind::Copy
            } else {
                TransferKind::Move
            };
            // Destino ortodoxo: el DIRECTORIO del otro pane. Los orígenes son
            // las marcas, o el cursor si no hay ninguna (#103). El resto —
            // nombre editable con un solo ítem (#105), confirm de lista con
            // varios— lo decide `open_transfer`, que es la MISMA puerta por
            // la que entra un drop del ratón: una segunda ruta para someter
            // una transferencia es una ruta que se queda sin confirmación,
            // sin colisiones o sin undo en cuanto una de las dos cambie.
            // Sin destino designado y con más de dos paneles, no se adivina:
            // una copia hacia un panel que el lector no tenía en la cabeza es
            // pérdida de datos silenciosa (ADR 0058 D7).
            // Sin candidato al rol `target` la operación PREGUNTA (spec L1):
            // con un solo listado no hay «el otro panel», y con tres o más no
            // se adivina cuál — en los dos casos se teclea la dirección en vez
            // de fallar. Adivinarla sería pérdida de datos silenciosa
            // (ADR 0058 D7); callarse, una tecla muerta.
            if let Some(dest) = app.target_index() {
                app.open_transfer(kind, app.focus(), dest, None);
            } else {
                app.open_transfer_dest(kind);
            }
        }
        // #105: shift+F6 — rename in situ (Move al PADRE de `from`, nombre
        // editable). Correcto también en el pane virtual: el destino sale
        // del propio path del hit, no del dir del pane.
        Command::PaneRename => app.open_rename(),
        // #106: Ctrl+R — recarga manual. Reusa el refresh post-mutación
        // (cancelable regla 3; marcas sobreviven vía refill con poda
        // VISIBLE, cursor por índice; el pane virtual de búsqueda se salta
        // — sus hits no viven en un dir). Ambos panes, como tras una task
        // propia: un cambio externo raramente respeta el foco.
        // #118: el desenlace VIAJA al run loop (`Cd::Refreshed`) — dispatch
        // no ve `fill`/`last_probed`, y sin el ritual un drenador paginado
        // vivo duplicaría filas sobre el listado recién completo.
        Command::PaneRefresh => {
            cd_outcome = Cd::Refreshed(refresh_panes(app, backend, events).await);
        }
        // Insert/Ctrl+A/Ctrl+Shift+A/`*` (#103): mc/Total Commander —
        // togglear la marca de esta entrada y avanzar (mantener Insert barre
        // un rango). Review MAJOR: bajo un quick search en Filter,
        // `toggle_mark` actúa sobre la selección FILTRADA mientras el cursor
        // real es otra cosa — avanzar el cursor real desincroniza el rango
        // barrido del filtro. La composición completa (marcar + a qué avanza
        // según haya o no filtro, clampado sin envolver) vive en el modelo
        // compartido.
        Command::MarkToggle => app.focused_mut().toggle_mark_and_advance(),
        Command::MarkAll => app.focused_mut().mark_all(),
        Command::MarkInvert => app.focused_mut().invert_marks(),
        Command::MarkClear => app.focused_mut().clear_marks(),
        // `+`/`-` (#103 T9): abren el modal de patrón (texto libre, ver el
        // brazo `app.modal.is_some()` de arriba) — marcar/desmarcar
        // corre al confirmar (`mark_pattern_confirm`), no aquí.
        Command::MarkPatternAdd => app.open_mark_pattern(true),
        Command::MarkPatternRemove => app.open_mark_pattern(false),
        // #104: F7 — crear directorio en el pane con foco. En el pane
        // VIRTUAL de búsqueda no hay directorio destino visible (review
        // MINOR-2: `dir()` es la raíz del walk, no lo que se pinta).
        Command::PaneMkdir => {
            if app.focused().virtual_search {
                app.message = Some(t("msg-mkdir-in-search"));
            } else {
                app.open_mkdir();
            }
        }
        // M4-IA: rename asistido del dir con foco. En el pane VIRTUAL de
        // búsqueda no hay un directorio único que renombrar (mismo criterio
        // que `PaneMkdir`). Las teclas del prompt y la petición viven en el
        // run loop (intercepción Tier-A + `AiRenameRun`).
        Command::PaneAiRename => {
            if app.focused().virtual_search {
                app.message = Some(t("msg-ai-rename-in-search"));
            } else {
                app.open_ai_rename();
            }
        }
        // M4-IA-2: búsqueda semántica sobre el índice (todos los roots). En
        // el pane VIRTUAL de búsqueda el prompt colisionaría con la
        // semántica Esc/Enter propia del modo (mismo criterio que
        // `PaneAiRename`). Las teclas del prompt y la petición viven en el
        // run loop (intercepción Tier-A + `SemanticRun`).
        Command::PaneSemanticSearch => {
            if app.focused().virtual_search {
                app.message = Some(t("msg-semantic-in-search"));
            } else {
                app.open_semantic_search();
            }
        }
        Command::PaneDelete | Command::PaneDeletePermanent => {
            // F8 = papelera si el provider la declara; sin ella, el MISMO
            // diálogo avisa de PERMANENTE (degradación con usuario
            // informado, ADR 0009). shift+f8 = permanente. La capability se
            // sondea UNA vez POR LOTE con el primer ítem (#103 T10): todas
            // las marcas viven en el mismo directorio del mismo provider,
            // así que N sondeos serían N round-trips de red para la misma
            // respuesta.
            if let Some(first) = app.focused().marked_paths().first() {
                let has_trash = backend
                    .capabilities(first)
                    .await
                    .is_ok_and(|c| c.flags.contains(norte_proto::CapabilityFlags::TRASH));
                let permanent = cmd == Command::PaneDeletePermanent || !has_trash;
                app.open_delete_modal(permanent);
            }
        }
        Command::PaneView => {
            // También symlinks (mismo criterio que nav.enter): si apunta a
            // un dir, el read fallará con mensaje visible.
            let target = app
                .focused()
                .selected()
                .filter(|e| matches!(e.kind, EntryKind::File | EntryKind::Symlink))
                .map(|e| e.path.clone());
            if let Some(path) = target {
                open_viewer(app, backend, events, path).await;
            }
        }
        // #140: elegir de `connections.toml`. Leer el fichero es del frontend
        // —el selector no toca disco— y navegar, del run loop.
        Command::PaneConnect => {
            let dir = norte_core::connect::config_dir();
            match norte_core::connect::named_connections(&dir).await {
                Ok(filas) => app.open_connections_picker(
                    filas
                        .into_iter()
                        .map(|(name, url)| norte_frontend::connections_picker::Row { name, url })
                        .collect(),
                ),
                Err(e) => app.message = Some(error_message(&e)),
            }
        }
        // Y desconectar SUELTA la sesión, no solo se va del panel: si no, el
        // socket seguiría abierto hasta que la sesión venciera sola y
        // «desconectar» sería un nombre para irse a otro sitio.
        Command::PaneDisconnect => disconnect(app, backend).await,
        Command::PaneOpen => resolve_opener(app),
        // #133: F4 EDITA. Lo ejecuta el run loop, como el shell y como
        // `pane.open`: es él quien tiene la terminal, y suspender la TUI para
        // devolvérsela a un programa de pantalla completa es exactamente lo
        // que ya hace `app.terminal`.
        //
        // La ruta viaja como ARGUMENTO y no dentro de una línea de comandos:
        // un nombre con una comilla, un `$` o un salto de línea o rompe la
        // línea o ejecuta parte de sí mismo, y aquí los nombres son bytes
        // (regla 1).
        Command::PaneEdit => match edit_under_cursor(app) {
            Ok(pendiente) => app.pending_shell = Some(pendiente),
            Err(msg) => app.message = Some(msg),
        },
        // Shift+F4: el editor con un buffer VACÍO en este directorio, que es
        // lo que hacen mc y Krusader. El nombre es cosa del editor —lo pide al
        // guardar—, y pedirlo aquí sería un diálogo que hace lo mismo peor.
        Command::PaneEditNew => match shell_cwd(app) {
            Ok(dir) => {
                app.pending_shell = Some(crate::app::PendingShell {
                    argv: vec![norte_frontend::shell::login_shell_editor()],
                    cwd: Some(dir),
                    wait_for_key: false,
                });
            }
            Err(msg) => app.message = Some(msg),
        },
        // #135 (S4, design §D): los tres se RESUELVEN aquí y los ejecuta el
        // run loop, que es el dueño de la terminal — mismo reparto que
        // `pane.open`. Nada de esto va al journal: un shell que abre el
        // usuario es el usuario actuando con sus permisos, no una mutación de
        // norte (no hay actor que atribuir ni reversa que grabar), y lo que
        // cambie en disco lo recoge el watcher y el refresh de la vuelta.
        Command::AppTerminal => match shell_cwd(app) {
            Ok(dir) => {
                app.pending_shell = Some(crate::app::PendingShell {
                    argv: vec![norte_frontend::shell::login_shell().into_os_string()],
                    cwd: Some(dir),
                    // El shell ya es interactivo: al salir de él, volver a
                    // los paneles es exactamente lo que se quiere.
                    wait_for_key: false,
                });
            }
            Err(msg) => app.message = Some(msg),
        },
        // Funciona en un pane remoto: no lanza nada ni mira el directorio —
        // solo enseña la terminal anfitriona hasta la siguiente tecla. Eso es
        // el SCROLLBACK, no el subshell vivo de mc: sin proceso persistente
        // detrás no hay nada en lo que escribir (issue #142).
        Command::AppTogglePanels => {
            app.pending_shell = Some(crate::app::PendingShell {
                argv: Vec::new(),
                cwd: None,
                wait_for_key: true,
            });
        }
        // Solo abre el prompt; el `$SHELL -c` lo deja pendiente su Enter, en
        // el run loop (que es quien lee las teclas crudas de un modal de
        // texto libre). El guard de localidad se repite ahí — el directorio
        // puede haber cambiado entre abrir el prompt y confirmarlo.
        Command::PaneCommandLine => match shell_cwd(app) {
            Ok(_) => app.open_command_line(),
            Err(msg) => app.message = Some(msg),
        },
        Command::ViewerClose => {
            // Con el preview acoplado, `viewer.close` SUELTA el teclado y deja
            // el panel donde está: cerrarlo es `layout.preview`. Cerrar un
            // panel que el lector solo quería dejar de manejar es la respuesta
            // equivocada, y es la misma regla que el sidebar.
            if app.key_owner() == crate::app::KeyOwner::Preview {
                app.return_keys_to_panes();
            } else {
                app.viewer = None;
            }
        }
        Command::ViewerUp => viewer_do(app, |v| v.scroll_up(1)),
        Command::ViewerDown => viewer_do(app, |v| v.scroll_down(1)),
        Command::ViewerPageUp => viewer_do(app, |v| v.scroll_up(crate::viewer::PAGE)),
        Command::ViewerPageDown => viewer_do(app, |v| v.scroll_down(crate::viewer::PAGE)),
        Command::ViewerTop => viewer_do(app, crate::viewer::Viewer::scroll_top),
        Command::ViewerBottom => viewer_do(app, crate::viewer::Viewer::scroll_bottom),
        Command::ViewerEncoding => viewer_do(app, crate::viewer::Viewer::cycle_encoding),
        Command::ViewerEncodingAuto => viewer_do(app, crate::viewer::Viewer::reset_encoding),
        Command::ViewerHex => viewer_do(app, crate::viewer::Viewer::toggle_hex),
        // H3c: la página de DONDE ESTÁ el lector, no el índice. Todo el cuerpo
        // vive en `open_contextual_help` (documentado allí) para que los tests
        // abran la ayuda por el MISMO sitio que F1.
        // H3e: la foto del catálogo se toma AQUÍ, en el camino de apertura —
        // una sola llamada, jamás mientras se pinta. Un fallo del backend deja
        // la ayuda sin filas de extensión (y con todo comando `plugin:`
        // atenuado), que es exactamente lo que «no lo pude averiguar»
        // significa; nunca tumba la ayuda entera.
        Command::AppHelp => {
            let plugins = backend.plugins_list().await.ok();
            open_contextual_help(app, lang, help_lines, plugins.as_ref());
        }
        Command::PaneNamesEncoding => {
            // #57: cicla la reinterpretación de nombres no-UTF8 del pane con
            // foco (display-only, regla 1). El anuncio va por la barra.
            let label = app.focused_mut().cycle_name_encoding();
            app.message = Some(match label {
                Some(enc) => ta("msg-names-encoding", &[("enc", enc)]),
                None => t("msg-names-encoding-off"),
            });
        }
        Command::PaneToggleHidden => {
            // #107: presentación-solo — el pane aparta/devuelve dotfiles,
            // el provider no re-lista. El anuncio va por la barra.
            let showing = app.focused_mut().toggle_hidden();
            app.message = Some(if showing {
                t("msg-hidden-shown")
            } else {
                t("msg-hidden-hidden")
            });
        }
        Command::AppTheme => app.open_theme_picker(),
        // ASÍNCRONA por lo mismo que `app.palette`: las filas de columna de
        // plugin salen de `plugin.list` (aprobado + activado). Un fetch
        // fallido NO impide abrir el picker — degrada a builtins + attrs,
        // igual que la palette degrada a built-ins.
        // #138: la misma semántica que un click en la cabecera
        // (`SortSpec::after_click`) — la columna activa invierte, una nueva
        // ordena ascendente— y sobre el pane con el FOCO, no sobre los dos: el
        // orden es de un listado, como el cursor.
        Command::PaneSortName => app.sort_focused_by(norte_frontend::SortColumn::Name),
        Command::PaneSortExt => app.sort_focused_by(norte_frontend::SortColumn::Extension),
        Command::PaneSortSize => app.sort_focused_by(norte_frontend::SortColumn::Size),
        Command::PaneSortTime => app.sort_focused_by(norte_frontend::SortColumn::Mtime),
        // El «menú de orden» es el diálogo de columnas: ahí está la columna,
        // la dirección y `dirs_first`, y `dialog.sort` ordena por la fila bajo
        // el cursor. Una segunda pantalla para lo mismo sería otra que
        // mantener y otra que aprender.
        Command::PaneSortMenu => {
            let plugins = backend
                .plugins_list()
                .await
                .map(|l| l.plugins)
                .unwrap_or_default();
            app.open_columns_picker(&plugins);
        }
        // #139: las propiedades salen del listado. Lo único que hay que pedir
        // es lo que un listado no sabe —cuánto ocupa una carpeta—, y se pide
        // solo si la entrada es una.
        Command::PaneProperties => {
            if let Some(dir) = app.open_properties() {
                // La fecha de una carpeta no viene en un listado perezoso
                // (#52) y un `stat` la sabe: se pide una vez, al abrir.
                if let Ok(fresca) = backend.stat(&dir).await {
                    app.properties_hydrate(fresca);
                }
                launch_size_count(app, backend, vec![dir], true).await;
            }
        }
        // Y contar a mano, sobre lo MARCADO (o el cursor si no hay marcas):
        // «¿cuánto ocupa todo esto?» es una pregunta sobre la selección.
        Command::PaneDirSize => {
            let targets = app.focused().marked_paths();
            launch_size_count(app, backend, targets, false).await;
        }
        // #132: escribir archivos. Los cinco comandos que los cuatro presets
        // atan y norte no tenía.
        Command::PanePack => app.open_pack(),
        Command::PaneSplitFile => app.open_split(),
        Command::PaneUnpack => unpack(app, backend).await,
        Command::PaneTestArchive => test_archive(app, backend).await,
        Command::PaneCombineFiles => combine_pieces(app, backend).await,
        Command::PaneColumns => {
            let plugins = backend
                .plugins_list()
                .await
                .map(|l| l.plugins)
                .unwrap_or_default();
            app.open_columns_picker(&plugins);
        }
        Command::AppExtensions => match backend.plugins_list().await {
            // El catálogo llega YA ordenado por categoría e id desde el core.
            Ok(list) => {
                // (P1 encoding audit F1) INGEST: clampa+enmascara `description`
                // UNA vez aquí, no en cada frame de `plugin_description_line`
                // — defensa contra un daemon hostil/comprometido que ignore
                // el tope del manifiesto.
                let mut plugins = list.plugins;
                crate::app::clamp_plugin_descriptions(&mut plugins);
                app.extensions = Some(ExtensionManager {
                    plugins,
                    errors: list.errors,
                    cursor: 0,
                    config: None,
                });
            }
            Err(e) => app.message = Some(error_message(&e)),
        },
        // Ctrl+P / vim `:` (H1 T4, spec-promised): abre la palette sobre la
        // snapshot PRECOMPUTADA (`App::palette_rows`, `main::build_keymaps`
        // + hot-reload) — jamás recalcula el keymap efectivo aquí. Elegir
        // `app.palette` DESDE la palette (el run loop la cierra ANTES de
        // despachar, `enter`) es un no-op observable: cierra y reabre
        // vacía — inofensivo, sin recursión de estado.
        //
        // (P1) ahora es ASÍNCRONA, como `app.extensions` arriba: las filas
        // de plugin necesitan `backend.plugins_list().await` (aprobado +
        // activado, `palette::plugin_rows`). A diferencia de `app.extensions`
        // (que NO abre el gestor si el fetch falla), los built-ins SIEMPRE
        // deben poder despacharse — un daemon caído no debe tumbar la
        // palette entera, solo degradarla (sin filas de plugin + un aviso),
        // mismo principio "un error de listado no tumba el TUI" del resto
        // de `dispatch`.
        Command::AppPalette => {
            // MINOR-6 (H1 close): Ctrl+P/`:` viven en `[global]`, fundido en
            // AMBOS efectivos — la palette puede abrirse desde el viewer
            // también, no solo desde browse (`rows_for_context` doc).
            let mut rows =
                crate::palette::rows_for_context(&app.palette_rows, app.viewer.is_some());
            match backend.plugins_list().await {
                Ok(list) => {
                    // (P1 encoding audit F1) INGEST: mismo clamp que el brazo
                    // `app.extensions` — un solo punto de entrada, mismo tope.
                    let mut plugins = list.plugins;
                    crate::app::clamp_plugin_descriptions(&mut plugins);
                    rows.extend(crate::palette::plugin_rows(&plugins));
                }
                Err(e) => app.message = Some(error_message(&e)),
            }
            app.palette = Some(Palette::new(rows));
        }
        // `F11` (S3): overlay de ajustes — las filas nacen del `cfg` VIGENTE
        // (mismo criterio que `help_lines`/`app.palette_rows`: reconstruidas
        // al abrir, jamás una copia arrastrada). Sección Plugins (G3c): un
        // resumen POR plugin con `[config]` (real ahora, ya no la nota
        // informativa de P2 — `plugin_config_summaries`).
        Command::AppSettings => {
            let summaries = plugin_config_summaries(backend).await;
            app.settings = Some(Settings::new(crate::settings::build_rows(cfg, &summaries)));
        }
        Command::TaskCancel => {
            app.message = Some(if app.board.cancel_last_running() {
                t("msg-cancelling")
            } else {
                t("msg-no-tasks")
            });
        } // Sin comodín (#112): `Command` es exhaustivo — un comando nuevo
          // sin brazo es un error de COMPILACIÓN, no un pánico de runtime.
    }
    cd_outcome
}
