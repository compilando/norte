//! The dispatch table: a command name (ADR 0006, the same ones the palette
//! and the wire see) and its effect.
//!
//! It lived in the `ntc` binary's root —a crate DIFFERENT from this lib—
//! and comes out WHOLE. It is a flat 109-arm `match` and is not split up:
//! this repository already argued in writing against splitting flat tables
//! (`norte-core/src/daemon/server.rs`), and the argument is the same here —
//! splitting an exhaustive match by topic trades a table the compiler checks
//! all at once for several that have to be kept in sync by hand.
//!
//! What makes it big is the vocabulary, not the depth: almost every arm is
//! one to three lines, and what each one calls already lives in its own
//! module — that is what this branch's nine previous rounds were pulling out
//! from underneath here.

use crate::app::{
    App, ExtensionManager, Modal, NavPopupKind, Palette, Settings, Trail, TrailStep, TransferKind,
    error_message,
};
use crate::config;
use crate::gestures::{
    EditLaunch, EnterAction, disconnect, edit_under_cursor, enter_action, mirror_plan,
    mirror_target_plan, pull_plan, resolve_opener, run_pane_gesture, shell_cwd,
};
use crate::keymap::Command;
use crate::mutations::{combine_pieces, launch_size_count, test_archive, unpack};
use crate::nav;
use crate::navigate::{Cd, cd, cd_in};
use crate::overlays::open_contextual_help;
use crate::refresh::refresh_panes;
use crate::screens::{open_drive_popup, plugin_config_summaries};
use crate::trail::walk_trail;
use crate::viewer_open::{open_viewer, viewer_do, viewer_sibling};
use norte_core::backend::Backend;
use norte_i18n::{t, ta};
use norte_proto::EntryKind;

/// Runs a named command (ADR 0006: the same names the palette and the wire
/// will see). A listing error on a cd does NOT bring the TUI down: the pane
/// stays where it was (visible warning: message bar, issue #20).
#[expect(
    clippy::too_many_lines,
    clippy::too_many_arguments,
    reason = "command→effect dispatch table, not an API"
)]
pub async fn dispatch(
    app: &mut App,
    backend: &Backend,
    events: &mut crate::console::Console<'_>,
    help_lines: &[ratatui::text::Line<'static>],
    // H3b: the negotiated language `Command::AppHelp` opens the corpus in.
    // See the same parameter on `run`.
    lang: norte_i18n::Lang,
    quick_mode: nav::Mode,
    confirm_quit: config::ConfirmQuit,
    // S3 (`app.settings`): the CURRENT config — read-only, to build the
    // overlay's rows when opening it (`crate::settings::build_rows`).
    cfg: &config::LoadedConfig,
    cmd: Command,
) -> Cd {
    // Only the cds (nav.enter/nav.parent) touch the background fill; the
    // rest of the commands leave it as it is (`Cancelled`).
    let mut cd_outcome = Cd::Cancelled;
    match cmd {
        // S2 (`[ui] confirm_quit`): ONLY this arm (the named dispatch of
        // `app.quit`, reachable by keymap AND by the palette) honors the
        // config and can open `Modal::ConfirmQuit`. The hardcoded
        // `app.quit = true`s from Ctrl+C scattered through the rest of this
        // file (each overlay has its own, documented in place) are the
        // emergency exit — they stay IMMEDIATE on purpose, never asking.
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
        // `pane.copy-path` (#286): the catalogue declared it `Live` from the
        // GPUI GUI, which implemented it; that one was retired and nobody
        // kept the command, so the catalogue was promising something nobody
        // did.
        //
        // Two paths, and the order matters: first the desktop's helper
        // (`wl-copy`, `xclip`), because it ANSWERS whether it worked; and if
        // there is none —the normal case in an SSH session— the OSC 52
        // sequence, which is an option a terminal has and a window does not.
        // A terminal that does not support it ignores it silently and there
        // is no way to ask it, so the message SAYS which path it took: that
        // is what turns an uncertainty into something the reader can verify
        // by pasting.
        Command::PaneCopyPath => {
            let paths = app.focused().marked_paths();
            if paths.is_empty() {
                app.message = Some(t("msg-nothing-selected"));
            } else {
                let bytes = norte_frontend::shell::clipboard_bytes(&paths);
                let n = paths.len().to_string();
                app.message = Some(match norte_frontend::shell::copy_to_clipboard(&bytes) {
                    norte_frontend::shell::ClipboardOutcome::Done(_) => {
                        ta("msg-paths-copied", &[("n", &n)])
                    }
                    norte_frontend::shell::ClipboardOutcome::NoHelper => {
                        app.pending_osc52 = Some(norte_frontend::shell::osc52(&bytes));
                        ta("msg-paths-copied-osc52", &[("n", &n)])
                    }
                    norte_frontend::shell::ClipboardOutcome::Failed => t("msg-clipboard-failed"),
                });
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
        // Toggle, and through the SAME place as the side panels: the bar is
        // the application's chrome, so opening it cannot mean two things
        // depending on where it is asked from.
        Command::AppMenu => app.toggle_menu(),
        Command::LayoutSplitH => app.layout_split(norte_frontend::layout::Dir::Horizontal),
        Command::LayoutSplitV => app.layout_split(norte_frontend::layout::Dir::Vertical),
        Command::LayoutFocusNext => app.layout_focus(1),
        Command::LayoutFocusPrev => app.layout_focus(-1),
        Command::LayoutCloseSlot => {
            if app.layout_close_slot() {
                // Said out loud, with the shortcut the current preset really
                // binds: closing a panel is easy to do by accident and hard
                // to undo if you do not know how. The same sentence as the
                // window.
                app.message = Some(norte_frontend::notes::slot_closed(
                    app.chord_split_h.clone().as_deref(),
                    lang,
                ));
            } else {
                app.message = Some(norte_i18n::t("msg-layout-last-panel"));
            }
        }
        Command::LayoutGrow => app.layout_resize(1),
        Command::LayoutShrink => app.layout_resize(-1),
        Command::LayoutEqualize => app.layout_equalize(),
        Command::LayoutFlip => app.layout_flip(),
        Command::LayoutSetTarget => app.layout_set_target(),
        // L3: opening the sidebar is the moment to request the volumes, and
        // the ONLY one along with unfolding its section. If it was already
        // open they are not requested again: that press only takes the
        // keyboard.
        //
        // The request is left flagged (`App::places_wants_drives`) and the
        // loop serves it: no I/O happens here, and that way the same path
        // works for the other places the panel appears without going
        // through this key.
        Command::LayoutPlaces => app.toggle_places(),
        // The docked viewer asks for nothing here: what it reads comes from
        // `preview::want` in the loop, against each frame's cursor.
        Command::LayoutPreview => app.toggle_preview(),
        Command::LayoutProcesses => app.toggle_processes(),
        Command::LayoutLog => app.toggle_log(),
        Command::LayoutDiskMap => app.toggle_disk_map(),
        // The timeline (phase 7): opening is placing the slot, and reading
        // the journal is done HERE, which is where the backend is — like the
        // connections selector reads its file in its own arm.
        //
        // A daemon with no journal answers `Unsupported` and it is SAID: an
        // empty panel and a panel that cannot exist do not read the same. It
        // still opens either way, because the slot is what gives it
        // somewhere to say it.
        Command::LayoutTimeline => {
            app.toggle_timeline();
            // Re-read EVERY time the key leaves the panel open, not just on
            // creating it: between closing it and opening it again anything
            // could have happened —in fact that is the normal case, because
            // what gets done gets done with the panel closed— and a history
            // showing an earlier snapshot is worse than an empty one: the
            // empty one is noticeable.
            if app.timeline_slot().is_some() {
                cargar_timeline(app, backend, None).await;
            }
        }
        // #362: the terminal panel. The key opens, gives the keyboard and
        // returns it; it does NOT close, because closing kills the reader's
        // shell.
        //
        // The shell is started here and not in `toggle_terminal` because
        // starting it is I/O —a pty and a process— and the layout's state
        // does not do that. If it fails, it is said and the slot stays: an
        // empty panel that explains why is better than a key that does not
        // respond.
        Command::LayoutTerminal => {
            if app.terminal.is_some() {
                // There is already a shell: this is just the keyboard coming
                // and going, and asks for no directory at all. Asking here
                // would leave the reader unable to get back to THEIR
                // terminal because they are looking at a remote panel.
                app.toggle_terminal();
            } else {
                // Starting it does ask for a local directory, and
                // `shell_cwd` already knows how to say why there is none —
                // it is the same gate as `app.terminal`, and answers no over
                // a remote panel.
                match shell_cwd(app) {
                    Ok(dir) => {
                        app.toggle_terminal();
                        // The real size is set by the paint as soon as it
                        // knows which rectangle it got; this one is the
                        // startup size and lasts as long as the first turn
                        // takes.
                        match crate::termpanel::abrir(&dir, (80, 24)) {
                            Ok(t) => {
                                // Recorded, like its two siblings and with
                                // the same "not journalled" written: a shell
                                // the reader opens is the reader acting with
                                // their own permissions, not a norte mutation
                                // —there is no actor to attribute and no
                                // reversal to record—. But starting a shell
                                // is the most privileged thing a frontend
                                // does, and the log panel is now a surface
                                // that gets looked at.
                                tracing::info!(
                                    "TUI opened a shell in a terminal panel \
                                     (not journalled: no actor, no reversal)"
                                );
                                app.terminal = Some(t);
                            }
                            // The slot stays open even if the shell does not
                            // start: an empty panel with the reason written
                            // reads better than a key that does nothing.
                            Err(e) => app.message = Some(e.to_string()),
                        }
                    }
                    Err(msg) => app.message = Some(msg),
                }
            }
        }
        // #136: the tree opens, focuses and closes like the sidebar. Its
        // content is requested by the run loop, one branch per turn.
        Command::PaneTree => app.toggle_tree(),
        Command::LayoutMetadata => app.toggle_metadata(),
        // The layouts directory's listing is done HERE, off the runtime, and
        // arrives already built at `App` (rule 2). A directory that cannot
        // be read gives an empty list: the five factory ones remain, which
        // is more than nothing.
        Command::LayoutPick => {
            // Listing AND READING, both off the runtime: each row of the
            // selector paints its screen, and reading it as the cursor
            // passes over would be I/O in the loop (#244 M2, rule 2). With
            // no config directory there are no user files: the five factory
            // ones remain. It used to fall back to `PathBuf::default()`,
            // which reads `./layouts/` of the current directory — meaning,
            // cloning a repo and pressing F9 (#244 m3).
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
        // The profiles. Listing the directory AND reading each one's
        // `norte.toml` —that is where the row's title and a broken one's
        // reason come from— both off the runtime, for the same reason as the
        // layouts (rule 2, #244). With no config directory there are no
        // profiles: empty list, and never `PathBuf::default()`, which reads
        // `./profiles/` of the current directory (#244 m3).
        Command::ProfilePick => {
            let perfiles = match config::user_config_dir() {
                Some(dir) => {
                    tokio::task::spawn_blocking(move || norte_frontend::config::read_profiles(&dir))
                        .await
                        .unwrap_or_default()
                }
                None => Vec::new(),
            };
            app.open_profile_picker(perfiles);
        }
        // #306: saving what is on screen as a profile. Only the prompt opens
        // here; disk is touched by Enter, in the run loop.
        Command::ProfileSaveAs => app.open_profile_save_as(),
        // `profile.next`/`profile.prev` cycle through the list WITHOUT
        // opening the selector, which is what whoever has two profiles and
        // alternates wants. The switch itself is done by the run loop
        // (task 4): here only which one is next gets decided.
        Command::ProfileNext | Command::ProfilePrev => {
            let perfiles = match config::user_config_dir() {
                Some(dir) => {
                    tokio::task::spawn_blocking(move || norte_frontend::config::read_profiles(&dir))
                        .await
                        .unwrap_or_default()
                }
                None => Vec::new(),
            };
            app.pending_profile = norte_frontend::profile_picker::next_profile(
                &perfiles,
                app.active_profile.as_deref(),
                matches!(cmd, Command::ProfileNext),
            );
        }
        // `pane.sync-nav`: the PERMANENT mirror. It navigates nothing by
        // itself —it switches the mode on or off and says so—; whoever
        // mirrors is the single point every navigation goes through.
        Command::PaneSyncNav => {
            app.sync_nav = !app.sync_nav;
            app.message = Some(t(if app.sync_nav {
                "msg-sync-nav-on"
            } else {
                "msg-sync-nav-off"
            }));
        }
        // `pane.mirror`: the location comes from the FOCUSED pane and the
        // other one travels.
        Command::PaneMirror => {
            let plan = mirror_plan(app);
            let origin = app.focus();
            cd_outcome = run_pane_gesture(app, backend, events, plan, origin).await;
        }
        // `pane.mirror-target`: the same gesture, but what travels is the
        // CURSOR's target (the folder under it, if it is one).
        Command::PaneMirrorTarget => {
            let plan = mirror_target_plan(app);
            let origin = app.focus();
            cd_outcome = run_pane_gesture(app, backend, events, plan, origin).await;
        }
        // `pane.pull`: the same gesture backwards — the location comes from
        // the OTHER pane and the focused one travels.
        Command::PanePull => {
            let plan = pull_plan(app);
            // The origin is the SAME "other panel" the plan resolved.
            let origin = app.target_index().unwrap_or_else(|| app.focus());
            cd_outcome = run_pane_gesture(app, backend, events, plan, origin).await;
        }
        // `pane.swap`: does NOT touch disk — both listings already existed
        // and only switch sides. The half `dispatch` does not see (fill in
        // flight, decoration fetches, probe dedup) travels to the run loop.
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
        // `/` (spec 2026-07-18): starts the quick search in the config's
        // mode. With one already active the keys get swallowed before the
        // resolver, so this arm only runs to OPEN it — no recursion.
        Command::PaneQuickSearch => app.focused_mut().quick_start(quick_mode),
        // `Alt+↓` / `Ctrl+D` (spec 2026-07-18): with the popup open its keys
        // get swallowed before the resolver (overlay pattern) — these arms
        // only run to OPEN it.
        Command::PaneHistory => app.open_nav_popup(NavPopupKind::History),
        Command::PaneHotlist => app.open_nav_popup(NavPopupKind::Hotlist),
        // Spec 2026-09-15 D6/D7: popular ones and a SIDE's history. The side
        // is `panes[0]`/`panes[1]`, like in `pane.select-drive-left/-right`.
        Command::PanePopular => app.open_nav_popup(NavPopupKind::Popular),
        Command::PaneHistoryLeft => app.open_side_history(0),
        Command::PaneHistoryRight => app.open_side_history(1),
        // D5: jumping to the point is a NORMAL navigation —it enters the
        // trail— so `nav.back` undoes the jump.
        Command::NavJumpBack => {
            let pane = app.focus();
            match norte_frontend::history::jump_target(&app.history[pane]) {
                Ok(dir) => cd_outcome = cd_in(app, backend, events, pane, dir, Trail::Record).await,
                Err(key) => app.message = Some(t(key)),
            }
        }
        Command::NavSetJumpPoint => {
            let pane = app.focus();
            let dir = app.panes[pane].dir().clone();
            app.history[pane].set_jump(dir);
            app.message = Some(t("msg-nav-jump-point-set"));
        }
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
        // Un directorio se navega; un FICHERO se abre —con su programa
        // asociado si está en este disco, y con el visor interno si no—, que
        // es lo que hace un gestor ortodoxo. Antes, sobre un fichero, esta
        // tecla no hacía nada y tampoco lo decía.
        Command::NavEnter => match enter_action(app) {
            EnterAction::Cd(dir) => cd_outcome = cd(app, backend, events, dir).await,
            // Subir por la fila `..` deja el cursor sobre el directorio del
            // que se sale, igual que la tecla dedicada (`NavParent`): es lo
            // que hace que subir y bajar sea reversible, y no puede depender
            // de con cuál de las dos se suba.
            EnterAction::Up(padre) => {
                let hijo = app.focused().dir().clone();
                app.focused_mut().set_pending_focus(hijo);
                cd_outcome = cd(app, backend, events, padre).await;
                if matches!(cd_outcome, Cd::Failed(_)) {
                    app.focused_mut().clear_pending_focus();
                }
            }
            EnterAction::OpenExternal => resolve_opener(app),
            EnterAction::View(path) => open_viewer(app, backend, events, path).await,
            EnterAction::Nothing => {}
        },
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
        // #310: el renombrado en lote sin IA. Abre la PLANTILLA; el plan lo
        // pide el run loop al confirmar, y lo revisa el mismo modal que ya
        // revisa el de la IA — lo que hace segura la operación no es de dónde
        // salieron los nombres.
        Command::PaneRenameBatch => {
            if app.rename_batch_names().is_empty() {
                app.message = Some(t("msg-rename-batch-nothing"));
            } else {
                app.open_rename_batch();
            }
        }
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
        // El resto de la familia «marcar moviéndose». El TAMAÑO de la página
        // sale del último frame PINTADO, como el `pane.page-down` de al lado:
        // una constante aquí marcaría un tramo distinto del que el cursor
        // recorre en cuanto la ventana no midiera eso.
        Command::MarkToggleUp => app.focused_mut().toggle_mark_and_retreat(),
        Command::MarkTogglePageDown | Command::MarkTogglePageUp => {
            let n = app.focused().page_step();
            let abajo = cmd == Command::MarkTogglePageDown;
            app.focused_mut().toggle_mark_page(n, abajo);
        }
        Command::MarkToTop => app.focused_mut().mark_to_top(),
        Command::MarkToBottom => app.focused_mut().mark_to_bottom(),
        Command::MarkAll => app.focused_mut().mark_all(),
        Command::MarkInvert => app.focused_mut().invert_marks(),
        Command::MarkClear => app.focused_mut().clear_marks(),
        // `+`/`-` (#103 T9): abren el modal de patrón (texto libre, ver el
        // brazo `app.modal.is_some()` de arriba) — marcar/desmarcar
        // corre al confirmar (`mark_pattern_confirm`), no aquí.
        Command::MarkPatternAdd => app.open_mark_pattern(true),
        Command::MarkPatternRemove => app.open_mark_pattern(false),
        // #313: la extensión de la entrada BAJO EL CURSOR. Sin nada bajo el
        // cursor, o sobre algo sin extensión, no marca nada y lo dice: marcar
        // «todo lo que tampoco tiene extensión» es otra regla que nadie pidió.
        Command::MarkExtensionAdd | Command::MarkExtensionRemove => {
            let añadir = cmd == Command::MarkExtensionAdd;
            let n = app.focused_mut().mark_same_extension(añadir);
            if n == 0 {
                app.message = Some(t("msg-mark-no-extension"));
            }
        }
        Command::MarkFiles => {
            app.focused_mut().mark_kind(false);
        }
        Command::MarkDirs => {
            app.focused_mut().mark_kind(true);
        }
        // La red del que pulsó «desmarcar todo» sin querer. Sin foto —ningún
        // gesto en bloque todavía, o un `cd` que se la llevó— se dice, en vez
        // de dejar el panel sin marcas fingiendo que eso era lo de antes.
        Command::MarkRestore => match app.focused_mut().restore_previous_marks() {
            Some(n) => app.message = Some(ta("msg-marks-restored", &[("n", &n.to_string())])),
            None => app.message = Some(t("msg-marks-nothing-to-restore")),
        },
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
        // Fase 8: organizar el directorio con foco. SIN prompt de
        // instrucción, a diferencia de renombrar — lo que se pide es «mira
        // este directorio y propón una forma», y una caja de texto vacía
        // delante sugeriría que hay algo que teclear. El plan llega al mismo
        // sitio que el de un plugin `organizer`, y se revisa igual.
        Command::PaneOrganize => {
            if app.focused().virtual_search {
                app.message = Some(t("msg-ai-rename-in-search"));
            } else {
                app.pending_organize = true;
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
                // Las inservibles van DETRÁS de las buenas y no mezcladas
                // (#365): lo primero que se ve es lo que sí lleva a algún
                // sitio, y lo que no vale queda abajo, visible y sin poder
                // elegirse. Hacerlas desaparecer dejaría al lector buscando
                // por qué falta una conexión que él escribió.
                Ok((filas, inservibles)) => app.open_connections_picker(
                    filas
                        .into_iter()
                        .map(|(name, url)| {
                            norte_frontend::connections_picker::Row::buena(name, url)
                        })
                        .chain(inservibles.into_iter().map(|(name, motivo)| {
                            norte_frontend::connections_picker::Row::inservible(name, motivo)
                        }))
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
            Ok(EditLaunch::Shell(pendiente)) => app.pending_shell = Some(pendiente),
            Ok(EditLaunch::Open(pendiente)) => app.pending_open = Some(pendiente),
            Err(msg) => app.message = Some(msg),
        },
        // #312: comparar DOS ficheros. Mismo reparto que editar —el run loop
        // es quien tiene la terminal— y el mismo tipo de resultado, porque el
        // trato es el mismo: `[ui] diff` puede ser una ventana, y el `diff -u`
        // por defecto es un programa de terminal cuya salida hay que sostener.
        Command::PaneCompareFiles => match crate::gestures::compare_files(app) {
            Ok(EditLaunch::Shell(pendiente)) => app.pending_shell = Some(pendiente),
            Ok(EditLaunch::Open(pendiente)) => app.pending_open = Some(pendiente),
            Err(msg) => app.message = Some(msg),
        },
        // Shift+F4: un fichero VACÍO en este directorio y el editor encima.
        //
        // El nombre se pide AQUÍ y el fichero lo crea el daemon (`fs.create`,
        // #290), no el editor al guardar. Dejárselo al editor —lo que hacía
        // esta tecla— creaba el fichero fuera de norte: sin pasar por la
        // política, sin entrada en el journal y sin undo (regla dura 4). Es
        // además lo que hace la ventana con este mismo comando.
        //
        // El guard es que el pane tenga forma NATIVA, y no el `shell_cwd` del
        // shell: aquel falla por dos motivos —pane remoto, o local sin cwd
        // válido para un hijo (Windows, ruta que solo existe con `\\?\`)— y el
        // segundo no aplica aquí. Crear no necesita cwd, y el editor lo lleva
        // como best-effort, así que gatear con él habría rechazado del todo un
        // `edit-new` en una ruta larga de Windows.
        Command::PaneEditNew => {
            if norte_vfs_local::vpath_to_native(app.focused().dir()).is_ok() {
                app.open_edit_new();
            } else {
                app.message = Some(crate::gestures::shell_remote_message(app));
            }
        }
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
                    check_regular: None,
                });
            }
            Err(msg) => app.message = Some(msg),
        },
        // Fase 9: el RELEVO a la ventana. Aquí sólo se COMPRUEBA y se pide;
        // volcar la pantalla, soltar la sesión y lanzar la ventana son tres
        // viajes que no caben en el despacho de una tecla, así que los hace el
        // escritor de sesión y el run loop (`request_handoff`).
        //
        // Los dos impedimentos se dicen ANTES, con su motivo, y son los
        // mismos que atenúan la fila en la paleta y en la hoja de referencia:
        // que la tecla no haga nada es tolerable; que suelte la pantalla y
        // lance una ventana que nadie va a ver, no.
        Command::AppHandoff => {
            if !app.backend_daemon {
                app.message = Some(t("msg-handoff-needs-daemon"));
            } else if !app.has_desktop {
                app.message = Some(t("msg-handoff-needs-desktop"));
            } else {
                app.pending_handoff = true;
            }
        }
        // #142: el SUBSHELL de mc, no el scrollback. Aquí solo se PIDE; lo
        // arranca —perezosamente, la primera vez— y le cede la pantalla el run
        // loop, que es el dueño de la terminal. Mismo reparto que los tres de
        // arriba y por la misma razón.
        //
        // A diferencia de `app.terminal`, el directorio NO se resuelve aquí:
        // el shell ya existe entre una pulsación y la siguiente, y a dónde va
        // se decide al cederle la terminal.
        Command::AppTogglePanels => app.pending_subshell = true,
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
                // `close_viewer` limpia el visor Y su miniatura a la vez
                // (hallazgo de revisión, T3 fase 5): un `app.viewer = None`
                // suelto aquí dejaba `app.viewer_imagen` apuntando a la
                // imagen anterior mientras el visor ya no estaba.
                app.close_viewer();
            }
        }
        Command::ViewerUp => viewer_do(app, |v| v.scroll_up(1)),
        Command::ViewerDown => viewer_do(app, |v| v.scroll_down(1)),
        Command::ViewerPageUp => viewer_do(app, |v| v.scroll_up(crate::viewer::PAGE)),
        Command::ViewerPageDown => viewer_do(app, |v| v.scroll_down(crate::viewer::PAGE)),
        Command::ViewerTop => viewer_do(app, crate::viewer::Viewer::scroll_top),
        Command::ViewerBottom => viewer_do(app, crate::viewer::Viewer::scroll_bottom),
        Command::ViewerLeft => viewer_do(app, |v| v.scroll_left(1)),
        Command::ViewerRight => viewer_do(app, |v| v.scroll_right(1)),
        Command::ViewerEncoding => viewer_do(app, crate::viewer::Viewer::cycle_encoding),
        Command::ViewerEncodingAuto => viewer_do(app, crate::viewer::Viewer::reset_encoding),
        Command::ViewerHex => viewer_do(app, crate::viewer::Viewer::toggle_hex),
        Command::ViewerZoomIn => viewer_do(app, crate::viewer::Viewer::zoom_in),
        Command::ViewerZoomOut => viewer_do(app, crate::viewer::Viewer::zoom_out),
        Command::ViewerZoomFit => viewer_do(app, crate::viewer::Viewer::zoom_fit),
        Command::ViewerNext => viewer_sibling(app, backend, events, true).await,
        Command::ViewerPrev => viewer_sibling(app, backend, events, false).await,
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
        // #314: cambiar los permisos. El operando es el de siempre —lo
        // marcado, o el cursor—, y el campo se prellena con el modo de la
        // entrada bajo el cursor: teclear sobre un campo vacío es cómo se le
        // quita el bit de ejecución a algo que lo tenía.
        //
        // El `stat` con `posix.mode` se pide AQUÍ y no se saca del listado:
        // los listados de norte son perezosos (#52) y el modo no viaja en
        // ellos salvo que alguien lo pida.
        Command::PaneChmod => {
            let targets = app.focused().marked_paths();
            if targets.is_empty() {
                app.message = Some(t("msg-nothing-selected"));
            } else {
                let cursor = app.focused().selected().map(|e| e.path.clone());
                let modo = match cursor {
                    Some(p) => backend
                        .stat_attrs(&p, &["posix.mode".to_owned()])
                        .await
                        .ok()
                        .and_then(|e| norte_frontend::chmod::mode_of(&e)),
                    None => None,
                };
                app.open_chmod(targets, modo);
            }
        }
        // Y contar a mano, sobre lo MARCADO (o el cursor si no hay marcas):
        // «¿cuánto ocupa todo esto?» es una pregunta sobre la selección.
        Command::PaneDirSize => {
            let targets = app.focused().marked_paths();
            launch_size_count(app, backend, targets, false).await;
        }
        // #311: las sumas. Calcular es sobre lo marcado (el operando de
        // siempre); comprobar es sobre el fichero de sumas bajo el cursor, y
        // resuelve los nombres contra SU directorio.
        Command::PaneChecksum => {
            let paths = app.focused().marked_paths();
            if paths.is_empty() {
                app.message = Some(t("msg-nothing-selected"));
            } else {
                app.pending_checksum = Some(crate::app::ChecksumRequest::Compute { paths });
            }
        }
        Command::PaneChecksumVerify => match app.focused().selected().map(|e| e.path.clone()) {
            Some(sums) => {
                app.pending_checksum = Some(crate::app::ChecksumRequest::Verify { sums });
            }
            None => app.message = Some(t("msg-nothing-selected")),
        },
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
                // Y el catálogo que el gestor acaba de traer vuelve a declarar
                // los paneles (fase 3): aprobar un plugin aquí tiene que
                // colocar su panel en esta sesión, y desactivarlo tiene que
                // quitarlo. Pedirlo solo al arrancar dejaba lo segundo sin
                // pasar nunca.
                app.kinds.insert_panels(&plugins);
                app.extensions = Some(ExtensionManager {
                    plugins,
                    errors: list.errors,
                    cursor: 0,
                    foco: crate::app::ExtFoco::Lista,
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
                    // Mismo catálogo, misma declaración de paneles que en el
                    // brazo del gestor (fase 3).
                    app.kinds.insert_panels(&plugins);
                    rows.extend(crate::palette::plugin_rows(&plugins));
                }
                Err(e) => app.message = Some(error_message(&e)),
            }
            app.palette = Some(Palette::with_recent(rows, &app.palette_recent));
        }
        // «Ir a cualquier sitio» (fase 6). Las conexiones se leen AQUÍ, como
        // las lee `pane.connect`, y por lo mismo: tocar disco es del run
        // loop, no del modelo. Si no se pueden leer, se abre igual con una
        // sección menos — la pantalla que junta seis listas no se cae porque
        // una falte, y decirlo en la barra taparía lo que el lector vino a
        // hacer.
        Command::AppGoto => {
            let dir = norte_core::connect::config_dir();
            // Aquí SOLO las que llevan a algún sitio: «ir a cualquier sitio»
            // es una lista de destinos, y una entrada inservible no lo es. El
            // sitio donde se dice qué le pasa es el selector de conexiones
            // (#365), que es adonde el lector va a arreglarla.
            let (conexiones, _inservibles) = norte_core::connect::named_connections(&dir)
                .await
                .unwrap_or_default();
            crate::goto::abrir(app, &conexiones);
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
        }
        // La misma tarea que cancelaría, y el sentido por su estado EN VIVO
        // (ADR 0147). Contra un daemon que no sabe pausar se DICE: una pausa
        // que no ocurre y no se dice es peor que no ofrecerla.
        //
        // La cola en serie (ADR 0149): el interruptor de la sesión, y mover
        // en la cola la tarea señalada del panel de procesos.
        Command::TaskQueue => {
            app.encolar = !app.encolar;
            app.message = Some(t(if app.encolar {
                "msg-queue-on"
            } else {
                "msg-queue-off"
            }));
        }
        Command::TaskUp | Command::TaskDown => {
            let arriba = cmd == Command::TaskUp;
            app.message = Some(match app.processes_selected() {
                None => t("msg-no-tasks"),
                Some(task) => match task.mover_en_cola(arriba).await {
                    Ok(()) => t("msg-queued-moved"),
                    Err(norte_proto::Error::Unsupported) => t("msg-queued-not-moved"),
                    Err(e) => error_message(&e),
                },
            });
        }
        // Repetir la transferencia que falló, con sus MISMAS opciones
        // (ADR 0148): el contexto ya se guardaba para el diálogo de colisión,
        // y sin esto un fallo de red obligaba a rehacer la operación a mano.
        Command::TaskRetry => {
            if let Some(r) = app.board.last_failed_retry() {
                app.message = Some(t("msg-retrying"));
                crate::mutations::submit_transfer(app, backend, r.kind, r.from, r.to, r.opts).await;
            } else {
                app.message = Some(t("msg-no-retry"));
            }
        }
        // La llamada va en su propia task y se espera POCO: contra un daemon
        // remoto colgado, esperarla aquí congelaría el bucle de la interfaz
        // hasta su plazo de treinta segundos. Si no contesta a tiempo se
        // queda el «pausando…», y el estado real llega por el progreso.
        Command::TaskPause => {
            app.message = Some(match app.board.last_running() {
                None => t("msg-no-tasks"),
                Some((task, _))
                    if !norte_frontend::tasks::pausable(task.progress().borrow().kind) =>
                {
                    t("msg-pause-not-this")
                }
                Some((task, pausada)) => {
                    let pedida = tokio::spawn(async move { task.set_paused(!pausada).await });
                    let espera = std::time::Duration::from_millis(300);
                    match tokio::time::timeout(espera, pedida).await {
                        Ok(Ok(Err(norte_proto::Error::Unsupported))) => t("msg-pause-unsupported"),
                        Ok(Ok(Err(e))) => error_message(&e),
                        _ if pausada => t("msg-resuming"),
                        _ => t("msg-pausing"),
                    }
                }
            });
        } // Sin comodín (#112): `Command` es exhaustivo — un comando nuevo
          // sin brazo es un error de COMPILACIÓN, no un pánico de runtime.
    }
    cd_outcome
}

/// Trae una página de la línea de tiempo y la mete en su hueco (fase 7).
///
/// `desde` es el cursor: `None` para la primera —la más nueva— y el
/// `next_before_seq` de la anterior para seguir hacia atrás.
///
/// Un fallo se DICE en la barra y deja el panel como estaba. Los dos que se
/// esperan de verdad son un daemon sin journal (`Unsupported`) y uno que no
/// conoce el método, y los dos significan lo mismo para el lector: aquí no
/// hay historial que enseñar. Un panel vacío sin explicación se lee como «no
/// has hecho nada», que es otra cosa.
pub async fn cargar_timeline(app: &mut App, backend: &Backend, desde: Option<i64>) {
    let Some(id) = app.timeline_slot() else {
        return;
    };
    match backend
        .journal_list(desde, crate::timeline::POR_PAGINA, None)
        .await
    {
        Ok(page) => {
            if let Some(t) = app.panes.timeline_mut(id) {
                if desde.is_none() {
                    // Una RELECTURA vuelve a la fila que tenía el cursor, si
                    // sigue: releer con el panel abierto no puede mover al
                    // lector de donde estaba.
                    let volver_a = t.selected().map(|r| r.seq);
                    *t = norte_frontend::timeline::Timeline::new(&page.rows, page.next_before_seq);
                    if let Some(seq) = volver_a
                        && let Some(i) = t.rows().iter().position(|r| r.seq == seq)
                    {
                        t.set_cursor(i);
                    }
                } else {
                    t.extend(&page.rows, page.next_before_seq);
                }
            }
        }
        Err(norte_proto::Error::Unsupported) => {
            app.message = Some(t("timeline-unavailable"));
        }
        Err(e) => app.message = Some(error_message(&e)),
    }
}
