//! Binario del TUI (fases 3–4 M1): loop de eventos async sobre el core
//! EMBEBIDO o contra el DAEMON (fase 3 M2, por `[daemon] mode` o
//! `--daemon`), con keymap engine (ADR 0006). Regla 7: solo cambia el
//! transporte.
//! `norte_tui::tty::init/restore` gestionan raw mode + pantalla alternativa
//! con hook de pánico incluido: la terminal del usuario JAMÁS queda rota.
//! Pintan sobre la terminal DE CONTROL (`tty.rs`), no sobre stdout: desde
//! `--pick` stdout lleva datos, no secuencias de escape.
#![forbid(unsafe_code)]

use std::collections::VecDeque;
use std::sync::Arc;

use anyhow::{Context, Result};
use crossterm::event::{Event, EventStream, KeyCode, KeyModifiers};
use futures::StreamExt;
use norte_core::TransferOptions;
use norte_core::backend::{Backend, ConnEvent};
use norte_i18n::{t, ta};
use norte_proto::{Error, VPath};
#[cfg(test)]
use norte_tui::app::Pane;
use norte_tui::app::{
    App, CompareState, Modal, PAGE, Palette, SearchState, Trail, detail_for_bar, error_category,
    error_message,
};
// Con la tabla de despacho fuera, la producción del binario ya no nombra estos
// tres: los tres los nombraba ella. Sus `mod` de test los alcanzan por
// `super::`, así que van en la RAÍZ y explícitos bajo `cfg(test)` — el build de
// producción no ve ese uso y `cargo fix` los borraría (ya pasó dos veces).
#[cfg(test)]
use norte_tui::app::TrailStep;
use norte_tui::config::{self, Layers, WatchMode};
use norte_tui::config_reload::reload_config;
use norte_tui::dispatch::dispatch;
use norte_tui::event_loop::RunError;
use norte_tui::fill::{Fill, apply_fill_msg};
#[cfg(test)]
use norte_tui::navigate::Cd;
#[cfg(test)]
use norte_tui::overlays::open_contextual_help;
// `FillMsg` ya no se nombra en producción —quien lo construía y quien lo
// consumía se fueron con `fill.rs`—, pero cinco `mod` de test lo fabrican.
#[cfg(test)]
use norte_tui::fill::FillMsg;
use norte_tui::gestures::{keyboard_owner, launch_opener, submit_command_line};
use norte_tui::help::TuiChords;
use norte_tui::hints::DialogHints;
use norte_tui::jobs::{
    CompareRun, SearchRun, SyncRun, SyncTick, drain_compare, drain_search, drain_sync_plan,
    harvest_sync_apply, launch_compare, launch_search, launch_sync_apply, launch_sync_plan,
    on_compare_key, on_search_dialog_key, on_search_enter, on_search_escape, on_sync_key,
};
use norte_tui::keymap::{
    Command, Count, Resolution, Resolver, chord_from_crossterm, count_ignored_message,
    parse_plugin_key, unavailable_message,
};
use norte_tui::listing::initial_pane;
use norte_tui::lua::{
    CommandRun, RunOutcome, load_lua, refresh_lua_status, resolve_lua_trust, run_lua_command,
    start_lua_run,
};
use norte_tui::mouse;
use norte_tui::mutations::{on_dialog_key, submit_transfer};
use norte_tui::nav;
use norte_tui::navigate::{apply_cd, cache_capabilities, cd, cd_in, cd_landed_pane};
// Con el ritual de `cd` fuera, el binario ya no nombra estos cinco en
// producción: los llamaba el propio ritual. Sus seis `mod` de test todavía sí,
// y se irán con ellos.
#[cfg(test)]
use norte_tui::navigate::{first_page, needs_capabilities, reconcile_swap, record_step};
use norte_tui::overlays::{
    close_stale_overlays, fetch_plugin_page, help_owns_keys, modal_wins, palette_help,
    settle_help_over_modal, watch_refresh_allowed,
};
use norte_tui::paste::route_paste;
use norte_tui::probes::{
    CompareStatProbe, DecorateFetch, PreviewFetch, Probed, STAT_BATCH_MAX, STAT_WINDOW_RADIUS,
    StatProbe, spawn_compare_stat_probe, spawn_decorate_fetch, spawn_preview_fetch,
    spawn_stat_probe,
};
use norte_tui::refresh::{after_panes_refresh, on_tick, reap_search_run, refresh_panes};
use norte_tui::screens::{
    HelpDispatch, apply_theme, on_columns_key, on_connections_picker_key, on_extensions_key,
    on_help_key, on_layout_picker_key, on_nav_popup_key, on_places_key, on_processes_key,
    on_settings_key, on_theme_picker_key, on_tree_key, pane_attr_ids, refresh_places_drives,
    run_plugin_command,
};
use norte_tui::session_push::{
    JOURNAL_OCIOSO, SessionPush, captura_session, drena_avisos, push_session, restore_session,
};
use norte_tui::shortcuts_editor::{Maps, build_keymaps, on_shortcuts_key};
use norte_tui::suspend::run_suspended;
use norte_tui::trail::{nav_enter_target, nav_stalled};
use norte_tui::tty;
use norte_tui::ui;
use norte_vfs_local::LocalProvider;
use tokio_util::sync::CancellationToken;

/// Petición `ai.rename_plan` EN VUELO (M4-IA). Abortar el `JoinHandle`
/// cancela (regla 3): el abort dropea el future del backend en el runtime →
/// `CancelOnAbandon` envía `rpc.cancel` (remoto) / el timeout+drop aborta el
/// stream (embebido). OJO: DROPEAR el handle solo DESVINCULA la task de
/// tokio — cancelar exige `abort()` explícito.
struct AiRenameRun {
    /// La llamada al modelo, spawneada (es la única llamada larga del loop).
    handle: tokio::task::JoinHandle<Result<norte_proto::methods::AiRenamePlanResult, Error>>,
    /// Dir del pane al LANZAR; el plan se aplica AQUÍ aunque el usuario
    /// navegue mientras el modelo piensa.
    dir: VPath,
}

/// Un plan IA YA cosechado que espera a que se cierre el modal de turno
/// (M4-IA). Lleva el estado del plan del LOTE (§17), que se pide en cuanto
/// llega el plan IA: sin él, el modal abriría sin hash aprobado y confirmar
/// quedaría mudo hasta un segundo viaje que nadie dispara.
struct PendingAiPlan {
    /// Dir del pane al LANZAR (donde aterriza el lote).
    dir: VPath,
    /// Parejas from→to del modelo.
    entries: Vec<norte_proto::methods::AiRenameEntry>,
    /// Veredicto del lote: en vuelo, resuelto, o fallido.
    plan: norte_frontend::BatchPlan,
}

/// Petición `fs.rename_batch_plan` EN VUELO (§17). Spawneada por el mismo
/// motivo que [`AiRenameRun`]: es un `fs.list` del dir entero contra el
/// provider que toque, y esperarla dentro del `select!` dejaría el loop sin
/// dibujar, sin leer teclas y sin poder cancelar. A lo sumo una — el prompt
/// del rename IA no abre sobre otro modal, así que no hay dos planes IA
/// vivos a la vez que pudieran pisarse.
struct RenameBatchRun {
    /// La llamada al core, spawneada.
    handle: tokio::task::JoinHandle<Result<norte_proto::methods::FsRenameBatchPlanResult, Error>>,
}

/// Hits que pide la búsqueda semántica (M4-IA-2): compartido con la GUI
/// desde `norte-frontend` (la MISMA consulta debe devolver lo mismo en
/// ambos frontends); ver su doc para la relación con `SEMANTIC_HIT_LIMIT`
/// y el techo del server.
use norte_frontend::SEMANTIC_K;
use norte_frontend::layout::BySlot;

/// Petición `index.search_semantic` EN VUELO (M4-IA-2). Mismo contrato de
/// cancelación que [`AiRenameRun`] (regla 3): `abort()` dropea el future del
/// backend → `rpc.cancel` (remoto) / drop (embebido); DROPEAR el handle solo
/// desvincula. Sin dir capturado: la consulta va contra TODOS los roots del
/// índice (`root = None`), navegar mientras piensa no la invalida.
struct SemanticRun {
    /// La llamada al índice+modelo, spawneada.
    handle: tokio::task::JoinHandle<Result<Vec<norte_proto::methods::SemanticHit>, Error>>,
}

/// `F1` sobre una fila de la command palette (H3c): el puente hacia la página
/// que documenta ese comando, la otra dirección del que H3b ya tendió
/// (`Ctrl+P` desde la ayuda se lleva el filtro).

#[tokio::main]
#[allow(clippy::too_many_lines)] // wiring del binario, no API — mismo criterio que `run`/`dispatch`
async fn main() -> Result<()> {
    // Args: DIR posicional + `--preset`/`--daemon`/`--socket`. `--help` y
    // `--version` salen ANTES de tocar el terminal (antes se ignoraban como
    // flag desconocido y el binario moría al no poder abrir la TTY).
    let parsed = norte_frontend::cli::parse(std::env::args_os().skip(1), BOOL_FLAGS, VALUE_FLAGS);
    let Some(args) = args_or_exit(parsed)? else {
        return Ok(()); // `--help`/`--version`: ya impreso.
    };
    let (cli_preset, cli_layout, cli_daemon, cli_socket, cli_pick, cli_cd_file) = (
        args.text("--preset"),
        // `--layout` NO es texto por contrato: acaba siendo un nombre de
        // fichero, y por `to_string_lossy` dos bytes inválidos distintos
        // abrían el mismo `\u{FFFD}.toml` (#246 M1).
        args.os_text("--layout").map(std::ffi::OsString::from),
        args.has("--daemon"),
        args.path("--socket"),
        args.has("--pick"),
        args.path("--cd-file"),
    );
    let layers = config::standard_layers();
    let cfg = config::load_async(layers.clone())
        .await
        .context("config inválida")?;
    // Idioma: NORTE_LANG explícito > [ui] lang de la config > entorno.
    let lang = if std::env::var("NORTE_LANG").is_ok_and(|v| !v.is_empty()) {
        norte_i18n::Lang::from_env()
    } else if let Some(l) = &cfg.common.ui_lang {
        norte_i18n::Lang::negotiate(Some(l))
    } else {
        norte_i18n::Lang::from_env()
    };
    let _ = norte_i18n::force(lang);
    // Roadmap ítem 9: el log va al FICHERO y solo al fichero. Hasta aquí este
    // binario no instalaba subscriber ninguno y lo decía en un comentario más
    // abajo: un `fmt` a stderr pelea con la pantalla alternativa, así que cada
    // `tracing::warn!` de la TUI se descartaba mudo.
    //
    // Va DESPUÉS de cargar la config porque `[log] dir` sale de ella, lo que
    // significa que un `--help`/`--version` —que salen antes— no deja rastro.
    // Correcto: no hacen nada que merezca un log.
    //
    norte_core::logging::init_to_file(norte_core::logging::LogConfig {
        dir: cfg.common.log_dir.as_deref(),
        retain: cfg.common.log_retain,
    });
    let (browse_eff, viewer_eff, dialog_eff) = build_keymaps(&cfg, cli_preset.as_deref())?;
    // Bindings `lua:` descartados del keymap.toml de PROYECTO (seguridad,
    // review M4 Lua): se avisa tras crear la App, jamás descarte mudo. El
    // contexto `global` se fusiona en las TRES pantallas (H1 T2 suma
    // dialog), así que el máximo es el recuento sin dobles (un binding
    // global cuenta en todas).
    let discarded_lua = browse_eff
        .discarded_lua_bindings()
        .max(viewer_eff.discarded_lua_bindings())
        .max(dialog_eff.discarded_lua_bindings());

    let mut backend = make_backend(&cfg, cli_daemon, cli_socket).await?;

    // El DIR posicional manda sobre el `cwd`; se valida aquí para dar un
    // error claro en vez de un listado fallido dentro del TUI ya arrancado.
    let start = start_dir(args.dir)?;
    // #108 b4: columnas y orden desde `[ui.columns]` — resuelto UNA vez;
    // los ids inválidos no rompen el arranque (doctor los reporta). ANTES de
    // los listados iniciales (#117): ellos también piden los attrs
    // configurados — sin esto, las celdas attr nacen en blanco hasta el
    // primer cd/refresh.
    let columns = norte_frontend::columns::ColumnsSettings::resolve(&cfg.common.ui_columns);
    let start_attrs = columns.attr_ids_for(start.scheme());
    let left = initial_pane(&backend, &start, &start_attrs).await?;
    let right = initial_pane(&backend, &start, &start_attrs).await?;
    let mut app = App::new(left, right);
    app.pick = cli_pick; // `--pick` (S2): see the field's rustdoc (`app.rs`).
    app.columns = columns;
    // Sincronizar necesita journal Y spool (regla dura 4: `sync.apply` abre un
    // lote deshacible y se niega sin él; `sync.plan` se niega sin spool). Desde
    // #167 el brazo embebido SÍ lleva el journal del directorio de estado (que
    // desde #177 abre en su primera mutación), pero sigue sin spool, así que
    // `is_journalled()` sigue diciendo que no — y dice la verdad sobre lo único
    // que atenúa, que es sincronizar. Se decide UNA vez, aquí, porque el
    // `Backend` no cambia de brazo en vida del proceso.
    app.backend_journalled = backend.is_journalled();
    // #117: el catálogo del scheme de arranque — incondicional, como el cd
    // (una vez por scheme y sesión; el picker de la tarea 4 lo quiere
    // aunque no haya columnas attr configuradas); un fallo NO tumba el
    // arranque — sin catálogo se pinta con defaults Opaque.
    // H3d: la MISMA respuesta trae las caps (`fs.capabilities` devuelve las
    // dos mitades), así que se cachean juntas — sin ellas la ayuda del primer
    // F1 caería al criterio sintáctico teniendo el dato al alcance.
    if let Ok(both) = backend.capabilities_and_attrs(&start).await {
        cache_capabilities(&mut app, &start, both);
    }
    for i in 0..app.panes.len() {
        app.apply_scheme_sort(i);
    }
    // #107: `[ui] show_hidden` siembra el estado INICIAL de ambos panes;
    // Ctrl+H lo cambia por pane en runtime (el hot-reload no lo pisa — un
    // toggle del usuario no debe deshacerse porque otro campo cambió).
    if let Some(show) = cfg.common.ui_show_hidden {
        for pane in &mut app.panes {
            pane.set_show_hidden(show);
        }
    }
    // `[ui] layout`: una disposición guardada. Un layout que no carga NO deja
    // a norte sin pantalla — se avisa por la barra y se arranca con
    // `orthodox`, que es lo que el usuario tenía antes de escribir la clave.
    // `--layout` gana a `[ui] layout`: elegir una disposición para UN arranque
    // no debe tocar tu config, que es justo lo que hace la clave.
    let nombre_layout: Option<std::ffi::OsString> = cli_layout
        .clone()
        .or_else(|| cfg.common.ui_layout.clone().map(std::ffi::OsString::from))
        .filter(|n| n != std::ffi::OsStr::new("orthodox"));
    if let Some(nombre) = nombre_layout {
        // El fichero se lee FUERA del runtime (regla 2), y sin directorio de
        // config no hay fichero que valga: queda el preset de ese nombre.
        let cargado = match config::user_config_dir() {
            Some(dir) => {
                let n = nombre.clone();
                tokio::task::spawn_blocking(move || norte_frontend::layout::config::load(&dir, &n))
                    .await
                    .unwrap_or_else(|_| {
                        Err(norte_frontend::layout::LayoutError::NotFound(String::new()))
                    })
            }
            None => Err(norte_frontend::layout::LayoutError::NotFound(String::new())),
        };
        app.apply_loaded_layout(&nombre, cargado);
    }
    // L2: la pantalla que dejaste. Va DESPUÉS de `[ui] layout` a propósito —
    // una sesión guardada es más específica que una preferencia de config, y
    // es la que gana— y antes del tema, que no depende de ninguna de las dos.
    // Un fallo NO tumba el arranque: se sigue con la pantalla de la config.
    restore_session(&mut app, &backend).await;
    apply_theme(&mut app, &cfg);
    // Copia de la hotlist en el App (spec 2026-07-18): la fuente del popup
    // `Ctrl+D`; se refresca en cada hot-reload OK (`reload_config`).
    app.hotlist = cfg.common.hotlist.clone();
    // Hints de pie de página de los overlays (H1 T3, #24): PRECOMPUTADOS del
    // efectivo `dialog` ANTES de que se mueva al `Resolver` de abajo — igual
    // que `help_lines`, se reconstruyen en cada hot-reload OK.
    app.dialog_hints = DialogHints::build(&dialog_eff);
    // Openers declarativos (#28): fuente de `pane.open` (F4).
    app.openers = cfg.openers.clone();
    // Canales del modo daemon (None en embebido): tasks de otros frontends
    // y avisos de (re)conexión — se drenan en el loop principal.
    let foreign_tasks = backend.take_foreign_tasks();
    let conn_events = backend.take_conn_events();
    let approvals = backend.take_approvals();
    // #44: avisos `connection.degraded` del daemon → indicador persistente.
    let degraded = backend.take_degraded();
    // #167/#177: el brazo embebido abre el journal en su primera mutación, y si
    // resulta que lo tiene otro proceso, esta sesión muta SIN registro. Eso se
    // dice EN la sesión y en el instante en que ocurre: un `eprintln!` de
    // arranque lo taparía la pantalla alternativa un segundo después, y aquí ni
    // siquiera se sabe al arrancar. (Un indicador permanente en la barra sería
    // mejor que un mensaje que el siguiente borra; sigue pendiente.)
    let journal_warnings = backend.take_journal_warnings();
    let mut help_lines = norte_tui::help::build(&browse_eff, &viewer_eff, &dialog_eff);
    // H3b: the chord resolver the help corpus is rendered through. Built from
    // the SAME three effectives as `help_lines` and BEFORE they move into the
    // `Resolver`s below (it borrows), and rebuilt alongside them on every hot
    // reload — the obligation `TuiChords`' own rustdoc states: a rebind that
    // does not reach this resolver is a page that teaches the OLD key.
    app.help_chords = Arc::new(TuiChords::new(&browse_eff, &viewer_eff, &dialog_eff, lang));
    // Filas de la command palette (H1 T4): PRECOMPUTADAS de los efectivos
    // browse/viewer ANTES de que se muevan al `Resolver` de abajo — mismo
    // criterio que `help_lines`/`dialog_hints`.
    app.palette_rows = norte_tui::palette::build_rows(&browse_eff, &viewer_eff);
    let mut resolver = Resolver::new(browse_eff);
    let mut viewer_resolver = Resolver::new(viewer_eff);
    // H1 T2: resolver compartido por TODOS los overlays (modal, theme
    // picker, extensions, nav popup) — mutuamente exclusivos en el run loop
    // (el `if`/`else if` de más abajo), así que un único estado de secuencia
    // basta. Los presets `[dialog]` son de UN chord; un `Resolution::Pending`
    // (solo posible con una secuencia multi-tecla de una capa de usuario) se
    // trata como ignorar-y-reiniciar en cada handler — sin semántica de
    // overlay definida para eso todavía.
    let mut dialog_resolver = Resolver::new(dialog_eff);

    // Hot-reload: vigilancia de las capas, con aviso si degrada a polling.
    let (cfg_tx, cfg_rx) = tokio::sync::mpsc::channel(8);
    let watch = config::watch(&layers, cfg_tx).await;
    if watch.mode == WatchMode::Polling {
        app.message = Some(t("msg-config-polling"));
    }
    // DESPUÉS del aviso de polling: el de seguridad no debe quedar pisado.
    if discarded_lua > 0 {
        app.message = Some(ta(
            "msg-lua-keymap-project",
            &[("n", &discarded_lua.to_string())],
        ));
    }

    let (tty_out, mut mouse_out) = open_terminal_or_exit()?;
    let mut terminal = tty::init(tty_out)?;
    let mut capture = arm_mouse(&cfg, &mut app, &mut mouse_out);
    let res = run(
        &mut terminal,
        &mut capture,
        &mut app,
        &backend,
        &mut resolver,
        &mut viewer_resolver,
        &mut dialog_resolver,
        &mut help_lines,
        lang,
        layers,
        cli_preset,
        cfg.quick_search_mode,
        cfg.common.ui_confirm_quit,
        cfg,
        cfg_rx,
        foreign_tasks,
        conn_events,
        approvals,
        degraded,
        journal_warnings,
    )
    .await;
    let _ = capture.set(false, terminal.backend_mut());
    restore_terminal(&mut terminal);
    drop(watch);
    res?; // A broken run loop is not a cancelled `--pick`.
    write_cd_file(&app, cli_cd_file.as_deref());
    finish_pick(&mut app);
    Ok(())
}

/// `--cd-file` (S3): writes the final directory for the `norte shell-init`
/// wrapper to read, on a clean quit only. A no-op when the flag was never
/// passed.
///
/// Placed after `res?`, not before: a run loop that returned an error bails
/// out of `main` right there and never reaches this call, so a crash writes
/// nothing to the cd-file — the design's own rule (§C: "the write happens at
/// the end", so the shell stays where it was).
///
/// Placed after [`restore_terminal`] too, deliberately DIFFERENT from the
/// design note's "before restoring the terminal": [`finish_pick`]
/// establishes, for the exact same shutdown window, that nothing must be
/// printed before the alternate screen is left or the terminal swallows it.
/// The `msg-cd-not-local` line below is exactly such a print, so it follows
/// `finish_pick`'s placement, not the design prose. Still runs BEFORE
/// `finish_pick` itself, whose `std::process::exit` would otherwise skip
/// this entirely when both `--pick` and `--cd-file` are given.
///
/// A write failure is printed and swallowed, the same shape as
/// `restore_terminal`'s own failure: `--cd-file`'s exit codes are not
/// contracted the way `--pick`'s are (design §B's 0/1/2 table is that flag's
/// alone), and nothing downstream is waiting on this process's exit code the
/// way a shell wrapper waits on `--pick`'s.
fn write_cd_file(app: &App, cd_file: Option<&std::path::Path>) {
    let Some(path) = cd_file else { return };
    if let Some(bytes) = norte_frontend::shell::cd_bytes(app.focused().dir()) {
        use std::io::Write as _;
        let wrote = std::fs::File::create(path)
            .and_then(|mut f| f.write_all(&bytes).and_then(|()| f.flush()));
        if let Err(e) = wrote {
            eprintln!("ntc: failed to write --cd-file: {e}");
        }
    } else {
        // Same masking convention as the CLI's own plain-text output
        // (`norte-cli/src/main.rs`'s semantic-search listing): `path_display`
        // gives the badge as a bool because a raw stderr line has no
        // styling to hang it on, so a hostile name is marked with a
        // leading `!` instead of colour.
        let (texto, hostil) = norte_frontend::path_display(app.focused().dir());
        let marcado = if hostil { format!("!{texto}") } else { texto };
        eprintln!("{}", ta("msg-cd-not-local", &[("path", &marcado)]));
    }
}

/// `--pick` (S2): the picker's exit, decided AFTER the terminal is restored
/// — never before, or the alternate screen swallows every byte (the whole
/// point of Task 1). Exit codes per the design's table: 0 accepted
/// (written), 1 cancelled (nothing written — `q`/`F10` under `--pick` never
/// populate `app.picked`), 2 reserved for the no-tty error in
/// [`open_terminal_or_exit`] and, here, a write failure the caller needs to
/// tell apart from "user picked nothing".
///
/// Returns normally only when `--pick` was never passed: every other path
/// exits the process directly, so `main` never reaches its own `Ok(())`
/// with a pick outstanding.
fn finish_pick(app: &mut App) {
    if let Some(paths) = app.picked.take() {
        use std::io::Write as _;
        let bytes = norte_frontend::shell::pick_bytes(&paths);
        let mut stdout = std::io::stdout();
        if let Err(e) = stdout.write_all(&bytes).and_then(|()| stdout.flush()) {
            eprintln!("ntc: failed to write the pick: {e}");
            std::process::exit(2);
        }
        std::process::exit(0);
    }
    if app.pick {
        std::process::exit(1);
    }
}

/// Deshace [`tty::init`]. Lo mismo que hacía `ratatui::restore()`: no hay
/// mucho que hacer si falla, así que se imprime y se sigue saliendo.
fn restore_terminal(terminal: &mut tty::Tui) {
    if let Err(e) = tty::restore(terminal) {
        eprintln!("ntc: failed to restore terminal: {e}");
    }
}

/// Abre la terminal DE CONTROL (`tty.rs`, no stdout — desde `--pick` stdout
/// lleva datos) y un segundo descriptor duplicado para `arm_mouse`, que se
/// llama antes de que `run` reciba la `Tui` y por tanto no puede tomar
/// prestado el handle que se mueve a [`tty::init`] (dueño único del
/// backend).
///
/// Sin controladora (cron, ambos extremos con pipe) es un error legible y
/// código de salida 2 — jamás un pantallazo de escapes en el pipe de quien
/// nos invocó.
fn open_terminal_or_exit() -> Result<(tty::TtyOut, tty::TtyOut)> {
    let out = match tty::open_controlling_terminal() {
        Ok(out) => out,
        Err(e) => {
            eprintln!("ntc: no controlling terminal: {e}");
            std::process::exit(2);
        }
    };
    let mouse_out = out
        .try_clone()
        .context("no se pudo duplicar el descriptor de la terminal")?;
    Ok((out, mouse_out))
}

/// Pide la captura de ratón si `[ui] mouse` no la desactiva (default ON).
///
/// Se pide ANTES de entrar al run loop y `main` la retira SIEMPRE al salir,
/// pase lo que pase: una terminal devuelta en modo ratón escupe secuencias
/// de escape en cuanto el usuario mueve el puntero, y para entonces ya no
/// queda nadie escuchándolas.
///
/// Un emulador que no acepte la secuencia no es motivo para no arrancar: se
/// sigue sin ratón y se dice por la barra, jamás en silencio (el usuario
/// hará click y no pasará nada).
fn arm_mouse(cfg: &config::LoadedConfig, app: &mut App, out: &mut tty::TtyOut) -> mouse::Capture {
    // El hook de pánico de `tty::init` ya suelta pantalla alternativa, raw
    // mode Y ratón sobre un handle propio — pero la captura de ratón es un
    // DECSET de la terminal ENTERA, no algo que se vaya con la pantalla, así
    // que se envuelve otra vez aquí por si esta función algún día arma algo
    // que `tty::init` no sepa deshacer. Mismo patrón: se toma el hook
    // vigente y se sustituye por uno que primero suelta el ratón y LUEGO lo
    // llama. El closure no puede tomar prestado `out` (el préstamo no
    // sobrevive a esta función), así que abre un handle nuevo a la terminal
    // de control en el momento del pánico — igual que hace `tty::init`.
    let previo = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if let Ok(mut tty_out) = tty::open_controlling_terminal() {
            let _ = crossterm::execute!(tty_out, crossterm::event::DisableMouseCapture);
        }
        previo(info);
    }));
    let mut capture = mouse::Capture::new();
    if let Err(e) = capture.set(cfg.common.ui_mouse.unwrap_or(true), out) {
        tracing::warn!(error = %e, "no se pudo activar la captura de ratón");
        app.message = Some(t("msg-mouse-capture-failed"));
    }
    capture
}

/// Flags booleanos del TUI.
const BOOL_FLAGS: &[&str] = &["--daemon", "--pick"];
/// Flags con valor del TUI.
const VALUE_FLAGS: &[&str] = &["--preset", "--layout", "--socket", "--cd-file"];

/// Texto de `--help`. En INGLÉS y sin Fluent a propósito: se imprime ANTES
/// de negociar el idioma (que sale de la config, que aún no se ha leído).
const USAGE: &str = "\
ntc — orthodox file manager, terminal frontend

Usage: ntc [OPTIONS] [DIR]

Arguments:
  [DIR]  Directory to start in (default: the current directory)

Options:
      --preset <NAME>    Keymap preset (orthodox|vim|cua); overrides norte.toml
      --layout <NAME>    Layout for this run (orthodox|simple|krusader|explorer|full,
                         or one of your own under `layouts/`); overrides norte.toml
      --daemon           Talk to the daemon instead of the embedded core
      --socket <PATH>    Daemon socket (default: $XDG_RUNTIME_DIR/norte/daemon.sock)
      --pick             print the selection, NUL-terminated, and exit
      --cd-file PATH     write the final directory here, NUL-terminated
                         (used by the `norte shell-init` wrapper)
  -h, --help             Print help
  -V, --version          Print version
";

/// Directorio de arranque como [`VPath`]: el `[DIR]` de la línea de
/// comandos si vino, si no el `cwd`. Se valida ANTES de tomar el terminal
/// para dar un error legible en vez de un listado fallido dentro del TUI ya
/// arrancado.
///
/// Un cwd UNC de Windows (`\\server\share`, `\\wsl$\…`) ya round-trip-ea:
/// `vpath_from_native` lo mete como primer segmento y `to_native` lo
/// restituye como base de la raíz del OS (#22). Uno irrepresentable da
/// error claro, jamás un panic.
fn start_dir(dir: Option<std::path::PathBuf>) -> Result<VPath> {
    let nativo = match dir {
        Some(d) => {
            let meta = std::fs::metadata(&d)
                .with_context(|| format!("no se puede abrir {}", d.display()))?;
            anyhow::ensure!(meta.is_dir(), "{} no es un directorio", d.display());
            std::path::absolute(&d).unwrap_or(d)
        }
        None => std::env::current_dir().context("cwd")?,
    };
    norte_vfs_local::vpath_from_native(&nativo)
        .map_err(|e| anyhow::anyhow!("{} no representable como VPath: {e}", nativo.display()))
}

/// Resuelve los argumentos "de salida inmediata": imprime `--help`/
/// `--version` (devolviendo `None`, el caller termina) y convierte un flag
/// desconocido en error. Antes `--help` caía en el brazo de "ignora" y el
/// binario seguía hasta intentar tomar la TTY, donde moría con un panic de
/// ratatui.
fn args_or_exit(args: norte_frontend::cli::Cli) -> Result<Option<norte_frontend::cli::Cli>> {
    if args.help {
        print!("{USAGE}");
        return Ok(None);
    }
    if args.version {
        println!("ntc {}", env!("CARGO_PKG_VERSION"));
        return Ok(None);
    }
    if let Some(flag) = &args.unknown {
        anyhow::bail!("unknown flag `{flag}` — try `ntc --help`");
    }
    Ok(Some(args))
}

/// La frase TRADUCIDA de «esta sesión no queda registrada» (#167/#177).
///
/// El texto de `NoJournal::text()` es para el log del operador y va en crudo;
/// esto es interfaz, y la interfaz de este binario pasa por Fluent.
///
/// Las dos ramas dicen cosas DISTINTAS desde #178: `Busy` es «esto pasó y no
/// quedó anotado» y `Failed` es «esto no ha pasado». Compartir frase era el
/// defecto.
fn journal_warning_i18n(why: &norte_core::embedded::NoJournal) -> String {
    use norte_core::embedded::NoJournal as N;
    match why {
        N::Busy => t("msg-journal-busy"),
        // `detail_for_bar` y no el `Display` crudo: el motivo es el error de
        // `sqlx`/`JournalError`, que trae párrafos enteros (los `Corrupt`) y
        // texto derivado de rutas del entorno. La barra de estado tiene un
        // saneador para exactamente esto y todo lo demás pasa por él.
        N::Failed(motivo) => ta(
            "msg-journal-refused",
            &[("motivo", &norte_tui::app::detail_for_bar(motivo))],
        ),
        // `#[non_exhaustive]`: un motivo nuevo no puede quedarse mudo — si
        // alguna vez lo hay, que al menos salga el texto del core.
        otro => otro.text(),
    }
}

/// Elige el transporte (regla 7): `--daemon` o `[daemon] mode = daemon`
/// conecta al socket (arrancando `norte daemon run` si hace falta);
/// cualquier otra cosa = embebido (arranque instantáneo, el default).
async fn make_backend(
    cfg: &config::LoadedConfig,
    cli_daemon: bool,
    cli_socket: Option<std::path::PathBuf>,
) -> Result<Backend> {
    let want_daemon = cli_daemon || cfg.common.daemon_mode == Some(config::DaemonMode::Daemon);
    if !want_daemon {
        // #167: el transporte embebido registra sus mutaciones (regla dura 4) en
        // EL journal del directorio de estado, el mismo que abre el daemon. Si
        // otro proceso tiene el lock exclusivo se sigue sin él, avisando — ver
        // `norte_core::embedded`.
        //
        // Construirlo NO abre el fichero (#177): un `ntc` que solo navega no le
        // quita el journal al daemon ni a un `norte audit`. El lock se toma en
        // la primera mutación, y el aviso —si lo hay— llega por el canal de
        // `take_journal_warnings`, ya dentro de la sesión.
        let engine = norte_core::embedded::engine_in(&norte_core::connect::config_dir());
        // #95.2: límites anti-bomba de archives desde `[archive]` (capas de
        // usuario, jamás la de proyecto). Antes de cualquier navegación: los
        // providers compuestos se cachean con los límites de su primer uso.
        // rust review item 3 (C1): la conversión override+saturación vivía
        // duplicada aquí y en `norte_core::archive_config` — un único home
        // en el core (`limits_from_overrides`) para que TUI y daemon jamás
        // diverjan en los límites anti-bomba.
        if let Some(limits) = norte_core::archive_config::limits_from_overrides(
            cfg.common.archive_max_entries,
            cfg.common.archive_max_decompressed_bytes,
            cfg.common.archive_max_nesting,
        ) {
            engine.set_archive_limits(limits);
        }
        // Ítem 11 del roadmap: el programa que lee los RAR, si la config fija
        // uno. Viene del mismo `cfg.common` que ya se cargó, así que no honra
        // la capa Project — y aquí eso no es una preferencia, es que un repo
        // ajeno no elige qué binario se lanza.
        engine.set_rar_delegate(
            cfg.common
                .archive_rar_delegate
                .as_ref()
                .map(std::path::PathBuf::from),
        );
        engine.register_provider(Arc::new(LocalProvider::os_root()));
        // Conexiones remotas (fase 6e): un path sftp://…/ftp://… navegable si
        // la host key ya es de confianza. La CONFIRMACIÓN TOFU interactiva
        // (modal con fingerprint) es UX pendiente — hoy un primer contacto
        // aparece como error con la huella; confírmalo con `norte connect`.
        engine.set_connector(Arc::new(norte_core::connect::ConnectionManager::new(
            norte_core::connect::config_dir(),
        )));
        // IA (M4-IA): opt-in; sin [ai] el backend degrada (Unsupported).
        //
        // Estos diagnósticos eran `eprintln!` y llevaban un comentario
        // explicando que un `tracing::warn!` aquí se descartaría mudo, porque
        // este binario no instalaba subscriber. Ya lo instala (`init_to_file`,
        // roadmap ítem 9), así que van al log como el resto — y sin escribir en
        // una pantalla que ratatui está a punto de tomar.
        match tokio::task::spawn_blocking(norte_core::ai::AiConfig::load).await {
            Ok(Ok(ai_cfg)) => {
                if let Some(pcfg) = ai_cfg.rename_provider_config().cloned() {
                    match norte_core::ai::resolve_and_build(
                        &pcfg,
                        norte_core::connect::config_dir(),
                    )
                    .await
                    {
                        Ok(provider) => engine.set_ai_provider(provider),
                        Err(e) => tracing::warn!(error = %e, "proveedor de IA no disponible"),
                    }
                }
                // Embeddings (M4-IA-2): proveedor propio, opt-in igual —
                // future-proofing del plan: la TUI embebida aún no lleva
                // índice (with_index es del daemon), el wiring es por paridad
                // para cuando lo gane.
                if let Some(w) = norte_core::ai::install_embed_provider(&engine, &ai_cfg).await {
                    tracing::warn!(aviso = %w, "proveedor de embeddings");
                }
                engine.set_ai_config(ai_cfg);
            }
            Ok(Err(e)) => tracing::warn!(error = %e, "[ai] inválido"),
            Err(e) => tracing::warn!(error = %e, "la carga de [ai] falló"),
        }
        return Ok(Backend::Embedded(Arc::new(engine)));
    }
    #[cfg(not(unix))]
    {
        let _ = (cli_socket, cfg);
        anyhow::bail!("el modo daemon no está disponible en Windows todavía (issue #33)");
    }
    #[cfg(unix)]
    {
        use norte_core::backend::remote::RemoteBackend;
        let socket = match cli_socket.or_else(|| cfg.common.daemon_socket.clone()) {
            Some(s) => s,
            None => tokio::task::spawn_blocking(|| norte_core::daemon::default_socket_path(None))
                .await
                .context("resolución del socket")?,
        };
        let exe = std::env::current_exe().context("current_exe")?;
        // El binario del daemon es `norte` (la CLI), no `norte-tui`: junto
        // al ejecutable actual dentro del mismo directorio de instalación.
        let daemon_bin = exe.with_file_name("norte");
        let mut spawn_cmd: Vec<std::ffi::OsString> =
            vec![daemon_bin.into(), "daemon".into(), "run".into()];
        spawn_cmd.push("--socket".into());
        spawn_cmd.push(socket.clone().into());
        let remote = RemoteBackend::connect(
            socket,
            Some(spawn_cmd),
            norte_proto::methods::ClientInfo {
                // El nombre del BINARIO, no el del crate: es lo que el daemon
                // registra y lo que un humano lee en un log o en `norte
                // doctor`, y ahí tiene que aparecer el programa que arrancó.
                name: "ntc".into(),
                version: env!("CARGO_PKG_VERSION").into(),
            },
        )
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))
        .context("no se pudo hablar con el daemon")?;
        Ok(Backend::Remote(remote))
    }
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)] // wiring del binario, no API
async fn run(
    terminal: &mut tty::Tui,
    // Captura de ratón: la crea `main` (dueño de la terminal) y la retira
    // al salir; aquí se ENCIENDE y se APAGA en caliente (`[ui] mouse`) y se
    // suelta alrededor de cada suspensión por opener externo.
    capture: &mut mouse::Capture,
    app: &mut App,
    backend: &Backend,
    resolver: &mut Resolver,
    viewer_resolver: &mut Resolver,
    dialog_resolver: &mut Resolver,
    help_lines: &mut Vec<ratatui::text::Line<'static>>,
    // H3b: the NEGOTIATED language (`NORTE_LANG` > `[ui] lang` > environment,
    // the same value handed to `norte_i18n::force`). The help corpus is
    // per-locale, so the overlay must open on the locale the rest of the UI
    // already speaks — `Lang::from_env()` here would hand a reader whose
    // `[ui] lang` says `es` an English corpus inside a Spanish UI. Fixed for
    // the session: `force` is called once, so the hot reload keeps this value.
    lang: norte_i18n::Lang,
    layers: Layers,
    cli_preset: Option<String>,
    // Modo del quick search (`[ui] quick_search`): vive en el run loop como
    // el preset CLI y se actualiza en el hot-reload de config.
    mut quick_mode: nav::Mode,
    // `[ui] confirm_quit` (S2): mismo patrón que `quick_mode` — vive en el
    // run loop, `applies_live` (solo afecta a `app.quit` NUEVOS, uno en
    // curso ya decidió) y se actualiza en el hot-reload.
    mut confirm_quit: config::ConfirmQuit,
    // S3 (`app.settings`): la config COMPLETA vive aquí, no solo los campos
    // sueltos de arriba — el overlay de ajustes necesita leer CUALQUIER
    // entrada del catálogo (`crate::settings::build_rows`), no una lista
    // fija. Se actualiza ENTERA en cada hot-reload OK (`reload_config`, al
    // final, tras aplicar todo lo demás — mismo criterio que `quick_mode`/
    // `confirm_quit`: solo si TODO aplicó).
    mut cfg: config::LoadedConfig,
    mut cfg_rx: tokio::sync::mpsc::Receiver<()>,
    mut foreign_tasks: Option<tokio::sync::mpsc::UnboundedReceiver<norte_core::backend::TaskRef>>,
    mut conn_events: Option<tokio::sync::mpsc::UnboundedReceiver<ConnEvent>>,
    mut approvals: Option<
        tokio::sync::mpsc::UnboundedReceiver<norte_proto::methods::PolicyApprovalRequired>,
    >,
    mut degraded: Option<
        tokio::sync::mpsc::UnboundedReceiver<norte_proto::methods::ConnectionDegraded>,
    >,
    mut journal_warnings: Option<
        tokio::sync::mpsc::UnboundedReceiver<norte_core::embedded::JournalStatus>,
    >,
) -> Result<(), RunError> {
    let mut events = EventStream::new();
    // Tick del panel de tasks: copia snapshots del watch (jamás bloquea).
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(100));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // L2: la sesión se escribe UNA vez por segundo, no por tecla. Su propio
    // tick y no el de 100 ms porque son dos ritmos distintos: el panel de
    // tareas mira un `watch` en memoria y esto acaba en un fichero.
    let mut session_tick = tokio::time::interval(std::time::Duration::from_secs(1));
    session_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut session_push = SessionPush::arranca(backend, app.session.revision);
    // Debounce del hot-reload SIN bloquear el loop (revisión fase 6): cada
    // evento de config empuja el deadline; el reload corre cuando vence.
    let mut reload_at: Option<tokio::time::Instant> = None;
    // Listados paginados rellenándose en background (ADR 0017): un hueco POR
    // PANE — los dos panes pueden estar paginando a la vez, y con un hueco
    // global el cd de uno mataba el drenador del otro (ver [`Fill`]).
    let mut fill: BySlot<Fill> = BySlot::new();
    // Por dónde sigue el barrido de `fill` (ver el brazo del `select!`).
    let mut fill_cursor: usize = 0;
    // Búsqueda viva en curso (liveSearch T6): a lo sumo una (el pane virtual
    // es uno). Molde `Fill`: se drena en el select y se suelta al salir.
    let mut search_run: Option<SearchRun> = None;
    // Comparación de directorios en curso (`Shift+F2`): a lo sumo una — el
    // panel de diferencias es uno. Mismo molde que `search_run`.
    let mut compare_run: Option<CompareRun> = None;
    // Sincronización en curso (`Ctrl+Y`): a lo sumo una — el panel es uno, y
    // aprobar un plan mientras otro se aplica sería aprobar a ciegas.
    let mut sync_run: Option<SyncRun> = None;
    // Petición ai.rename_plan en vuelo (M4-IA): a lo sumo una — relanzar
    // aborta la anterior. Se cosecha en el select y Esc (BROWSE) la cancela.
    let mut ai_rename_run: Option<AiRenameRun> = None;
    // Plan IA listo llegado con OTRO modal abierto: se RETIENE aquí (la cola
    // de `App` es específica de aprobaciones) y se abre en cuanto no haya
    // modal — jamás pisar (disciplina `open_next_pending`).
    let mut pending_ai_plan: Option<PendingAiPlan> = None;
    // Petición fs.rename_batch_plan en vuelo (§17): a lo sumo una, cosechada
    // en el select como `ai_rename_run`.
    let mut rename_batch_run: Option<RenameBatchRun> = None;
    // Búsqueda semántica en vuelo (M4-IA-2): mismo molde que `ai_rename_run`
    // — a lo sumo una, relanzar aborta la anterior, Esc (BROWSE) cancela.
    let mut semantic_run: Option<SemanticRun> = None;
    // Hits listos llegados con OTRO modal abierto: se RETIENEN aquí y se
    // abren en cuanto no haya modal (disciplina `pending_ai_plan`).
    let mut pending_semantic: Option<Vec<norte_proto::methods::SemanticHit>> = None;
    // Scripting Lua (M4, ADR 0026): host por capas con trust TOFU. Como
    // `fill`, el estado vive en el run loop. El run en vuelo (a lo sumo UNO:
    // el estado Lua es uno) se pollea inline en el select — `CommandRun` es
    // !Send y este future corre en block_on, jamás en spawn.
    let mut lua_host = load_lua(app, &layers).await;
    let mut lua_run: Option<(CommandRun, CancellationToken)> = None;
    let mut lua_queue: VecDeque<String> = VecDeque::new();
    // Sonda de stat on-focus (#52, listado lazy): a lo sumo una en vuelo,
    // dedup por (pane, path) — dos panes sobre el MISMO dir deben hidratar
    // cada uno la suya (no reintenta un stat fallido hasta cambiar
    // selección).
    let mut stat_probe: Option<StatProbe> = None;
    let mut last_probed: Probed = Probed::new();
    // Sonda de stat de la fila SELECCIONADA del panel de diferencias (#157):
    // mismo molde que `stat_probe`, a lo sumo una en vuelo. El dedup vive en
    // `App::compare_size_probed` y no en una variable local del run loop
    // (a diferencia de `last_probed`) porque `App::compare_size_probe_targets`
    // ya lo consulta para decidir qué falta por pedir.
    let mut compare_stat_probe: Option<CompareStatProbe> = None;
    // Fetch de decoraciones de plugin en vuelo (G3b, ADR 0037): a lo sumo
    // uno, molde de `stat_probe`/`fill`.
    let mut decorate_fetch: BySlot<DecorateFetch> = BySlot::new();
    // L3: una lectura de preview en vuelo por hueco, superseded al moverse.
    let mut preview_fetch: BySlot<PreviewFetch> = BySlot::new();
    // #106 (watching): vigilancia de los dirs visibles — notify con
    // fallback a sondeo (pitfall inotify). El conjunto vigilado se
    // re-sincroniza en CADA vuelta (diff barato, no-op sin cambios).
    // Regla 2, exención puntual (review MINOR-6): crear el watcher y los
    // watch()/unwatch() de rewatch son syscalls cortas inline (mismo
    // criterio documentado que el draw síncrono de ratatui más abajo);
    // solo corren al arrancar o al CAMBIAR de dir.
    let mut dir_watch = norte_frontend::watch::DirWatch::new();
    let mut dir_watch_alive = true;
    loop {
        dir_watch.rewatch(&watch_targets(app));
        if dir_watch.take_degraded_notice() {
            app.message = Some(t("status-watch-degraded"));
        }
        // #135: la suspensión se drena AQUÍ y en NINGÚN otro sitio. El opener
        // de #28 se lanza en tres puntos (el despacho de teclas, el de la
        // palette y el de la ayuda) porque cada uno tiene su propio
        // `continue`; una suspensión también la deja pendiente el Enter de
        // `Modal::CommandLine`, que vive en un cuarto brazo con su propio
        // `continue` — así que el sitio que los cubre a todos, presentes y
        // futuros, es la cabecera de la vuelta. Antes del draw: los paneles
        // que se repinten ya son los del listado refrescado.
        // `Shift+F2`: el despacho resolvió QUÉ comparar; el run loop es el
        // dueño del canal y de la Task, así que lanza. Mismo reparto que
        // `pending_shell`/`pending_open`, y en la misma cabecera de vuelta,
        // por la misma razón: los brazos que responden teclas tienen sus
        // propios `continue`.
        if let Some(params) = app.pending_compare.take() {
            launch_compare(app, backend, &mut compare_run, params).await;
        }
        // #149 y #164: ¿cabe en el destino, y sabe el destino sujetar lo que se
        // escriba en él? Las dos son I/O, así que el modal se abre SIN los
        // avisos y esta vuelta los rellena. El reparto es el de
        // `pending_compare`: el despacho decide QUÉ, el run loop lo pregunta.
        //
        // Las dos preguntas fallan de forma DISTINTA, y es deliberado.
        //
        // El espacio se traga el fallo: no poder enumerar volúmenes no puede
        // impedir una copia ni pintar una alarma, y «no lo sé» se dice callando
        // — ese es el contrato de `space::warning`.
        //
        // El confinamiento no. Ahí el silencio SIGNIFICA «este destino sujeta
        // sus escrituras», así que tragarse el fallo sería afirmarlo sin
        // saberlo: fail-open en una línea de seguridad. Si no se sabe, se
        // avisa (revisión de seguridad de W5 B).
        if let Some(check) = app.pending_dest_check.take() {
            let libre = match check.total {
                // Sin total no hay pregunta de espacio que hacer, y enumerar
                // volúmenes para tirar la respuesta es I/O por nada.
                None => None,
                Some(_) => backend
                    .volumes(false)
                    .await
                    .ok()
                    .and_then(|vols| norte_frontend::space::free_for(&check.to, &vols)),
            };
            let aviso_espacio =
                norte_frontend::space::warning(check.total, libre, norte_i18n::active());
            let aviso_confinamiento = match backend.capabilities(&check.to).await {
                Ok(caps) => norte_frontend::confine::warning(caps, norte_i18n::active()),
                Err(_) => norte_frontend::confine::warning(
                    norte_proto::Capabilities {
                        flags: norte_proto::CapabilityFlags::empty(),
                        max_path: None,
                    },
                    norte_i18n::active(),
                ),
            };
            if let Some(Modal::ConfirmTransfer { space, confine, .. }) = app.modal.as_mut() {
                *space = aviso_espacio;
                *confine = aviso_confinamiento;
            }
        }
        // `Ctrl+Y` / `s` / `m`: el despacho resolvió QUÉ sincronizar, y aquí
        // se lanza — mismo reparto que la comparación, en la misma cabecera de
        // vuelta y por la misma razón.
        if let Some(params) = app.pending_sync.take() {
            launch_sync_plan(app, backend, &mut sync_run, params).await;
        }
        // Y la aprobación, que es la SEGUNDA Task del mismo diálogo. Lo único
        // que viaja es el hash (ADR 0049).
        if let Some(hash) = app.pending_sync_apply.take() {
            launch_sync_apply(app, backend, &mut sync_run, &hash).await;
        }
        // #140: el panel que acaba de desconectar vuelve a casa por el mismo
        // `cd` que cualquier otra navegación, con su ritual de vuelta.
        if let Some(casa) = app.pending_disconnect_home.take() {
            let outcome = cd(app, backend, &mut events, casa).await;
            apply_cd(
                &app.panes,
                &mut fill,
                &mut decorate_fetch,
                &mut last_probed,
                &mut search_run,
                outcome,
            );
        }
        if let Some(pending) = app.pending_shell.take() {
            let norte_tui::app::PendingShell {
                argv,
                cwd,
                wait_for_key,
            } = pending;
            // Auditoría (review de S4): el journal NO ve nada de esto a
            // propósito (design §D), así que el rastro de que aquí hubo un
            // shell vive en el log. Sin la línea de comandos —es del usuario
            // y no tiene por qué acabar en un fichero— y con el programa a
            // secas.
            let lanzado = argv.first().map(|a| a.to_string_lossy().into_owned());
            tracing::info!(
                program = lanzado.as_deref().unwrap_or("(none)"),
                wait_for_key,
                "TUI suspended for a user-started program (not journalled: no actor, no reversal)"
            );
            // Nada que ejecutar = `app.toggle-panels`: solo enseña la
            // terminal anfitriona. Refrescar tras él costaría un re-listado
            // completo (remoto incluido) por una tecla que no toca el disco.
            let lanzo_algo = !argv.is_empty();
            if let Err(e) = run_suspended(terminal, capture, argv, cwd, wait_for_key).await {
                // `detail_for_bar`, jamás el `Display` crudo del OS (review
                // de S4, L1/m4): el sistema lo localiza por su cuenta, no
                // tiene tope y —si el error viene de un join roto— arrastra
                // el payload de un panic. Y se NOMBRA el programa, como hace
                // `msg-open-failed`: si no, un `$SHELL` borrado y una
                // pantalla alternativa que no cerró dan el mismo texto.
                app.message = Some(ta(
                    "msg-shell-failed",
                    &[
                        ("program", lanzado.as_deref().unwrap_or("-")),
                        ("error", &norte_tui::app::detail_for_bar(&e.to_string())),
                    ],
                ));
            }
            // Lo que el shell haya hecho en disco se ve al volver, por el
            // MISMO camino que `pane.refresh` (#118): refresh cancelable +
            // el ritual completo, jamás un `set_listing` a mano.
            //
            // GATEADO igual que el refresh del watcher (review de S4, M2):
            // `refresh_panes` polea `events` y se come toda tecla que no sea
            // Esc/Ctrl+C, y reinterpreta Esc como «abandona el refresh». Con
            // un modal delante —una aprobación de agente puede haberse
            // plantado al cerrarse el prompt— eso se traga la respuesta del
            // usuario hasta que la aprobación caduca. Si no se puede
            // refrescar ahora, el watcher (canal de capacidad 1) o el tick
            // lo hacen al cerrarse el overlay.
            if lanzo_algo && watch_refresh_allowed(app) {
                let refreshed = refresh_panes(app, backend, &mut events).await;
                after_panes_refresh(app, refreshed, &mut fill, &mut last_probed, &mut search_run);
            }
        }
        // Review MINOR-1: `over_modal` describe el modal que hay AHORA, no uno
        // que ya se contestó. Antes de plantar los modales retenidos de abajo,
        // que tienen que encontrar la bandera ya limpia.
        settle_help_over_modal(app);
        // Plan IA retenido (M4-IA): abre en cuanto el modal activo se cierra.
        // Las aprobaciones no compiten aquí: con la cola no vacía y sin modal,
        // `open_next_pending` ya habría abierto una al cerrarse el anterior.
        if app.modal.is_none()
            && let Some(pendiente) = pending_ai_plan.take()
        {
            app.modal = Some(Modal::AiRenamePlan {
                dir: pendiente.dir,
                entries: pendiente.entries,
                offset: 0,
                plan: pendiente.plan,
            });
        }
        // Hits semánticos retenidos (M4-IA-2): misma disciplina. Si el plan
        // IA de arriba acaba de abrir, el `is_none` los deja esperando.
        if app.modal.is_none()
            && let Some(hits) = pending_semantic.take()
        {
            app.modal = Some(Modal::SemanticHits {
                hits,
                offset: 0,
                cursor: 0,
            });
        }
        // Barra Lua en cada vuelta, ANTES del draw (cacheada en el host).
        refresh_lua_status(app, lua_host.as_ref());
        // H3b: la ayuda se MAQUETA para el terminal sobre el que va a
        // pintarse, justo antes del draw — el modelo acota su scroll contra
        // el número de líneas que salieron, y solo el render lo sabe (ver
        // `HelpView::refresh`). Cada vuelta, no solo al cambiar de tema: un
        // resize no pasa por ninguna tecla.
        if let Some(lang) = app.help.as_ref().map(|h| h.state.lang()) {
            // H3e: la página de un nodo de plugin se pide AQUÍ, bajo demanda y
            // una sola vez por overlay (`fetch_plugin_page`). Antes de
            // maquetar, para que la página recién llegada se pinte en ESTE
            // frame y no en el siguiente.
            fetch_plugin_page(backend, app).await;
            let size = terminal.size().map_err(RunError::Terminal)?;
            let (ancho, alto) = ui::help_body_size(
                ratatui::layout::Rect::new(0, 0, size.width, size.height),
                lang,
            );
            app.refresh_help(ancho, alto);
        }
        // La ventana de cada pane se reconcilia ANTES de pintar (#124 + el
        // scroll pegajoso): el cursor ya está donde lo dejó la tecla, así que
        // esto decide qué filas se ven y el draw las pinta. Hacerlo DESPUÉS
        // costaba un frame de retraso — el cursor podía caer fuera de la
        // ventana pintada, o sea desaparecer de la pantalla justo al llegar
        // al borde.
        {
            let s = terminal.size().map_err(RunError::Terminal)?;
            ui::before_frame(app, ratatui::layout::Rect::new(0, 0, s.width, s.height));
        }
        // Exención puntual de la regla 2: el draw escribe la terminal de
        // control síncronamente (patrón async oficial de ratatui; acotado,
        // runtime multi-thread).
        let pintado = terminal
            .draw(|f| ui::draw(f, app))
            .map_err(RunError::Terminal)?;
        if app.quit {
            // La última foto, y esperarla. El tick de un segundo se pierde lo
            // que pasó dentro de ese segundo, y salir es cuando más duele:
            // hasta aquí, cerrar norte justo después de un `cd` guardaba el
            // directorio anterior.
            //
            // Sin el gate del modal, a propósito: un modal abierto significa
            // «no guardes lo que estoy decidiendo», y aquí ya no se está
            // decidiendo nada — se está saliendo, y lo que hay que guardar es
            // dónde se estaba.
            drena_avisos(app, &mut session_push);
            let ultima = (!app.session.detached)
                .then(|| captura_session(app, &mut session_push))
                .flatten();
            session_push.cierra(ultima).await;
            return Ok(());
        }
        // #124: el alto REAL del viewport vuelve al modelo tras cada frame —
        // la paginación (`page_step`) y el radio de la sonda de stat salen de
        // ahí en vez de constantes que mienten en cualquier terminal que no
        // mida justo eso. Con el visor abierto son 0 filas (ningún pane
        // pintado) y el modelo vuelve a sus fallbacks.
        // El alto REAL del frame que se acaba de pintar: si la terminal cambió
        // de tamaño entre `before_frame` y el draw, este es el bueno, y de él
        // salen la paginación y el radio de la sonda de stat.
        ui::before_frame(app, pintado.area);
        // MISMO trato para la geometría del ratón: el draw es quien sabe
        // dónde cayó cada pane y con qué scroll, así que la devuelve al
        // modelo y el hit test resuelve contra la pantalla que el usuario
        // está mirando. Sin esto habría que recalcular el layout en cada
        // click, y un click resuelto contra un layout que no es el pintado
        // no falla ruidosamente: marca el fichero de al lado.
        mouse::after_frame(
            app,
            ui::pane_geometry(app, pintado.area),
            ui::tab_zones(app, pintado.area),
            ui::menu_zones(app, pintado.area),
            ui::places_zones(app, pintado.area),
        );
        // L3: el visor acoplado sigue al cursor del listado activo. Lo que se
        // pide sale de `preview::want`, que devuelve `None` cuando el hueco no
        // se colocó — cerrado, detrás de una pestaña, o colapsado por falta de
        // sitio. Por eso la suspensión de un hueco oculto no es una
        // comprobación que alguien pueda olvidarse de escribir: sin objetivo
        // no hay nada que pedir.
        {
            let res = ui::resolved_for(app, pintado.area);
            match norte_tui::preview::want(app, &res) {
                Some((slot, norte_tui::preview::Want::File(path))) => {
                    let ya = app
                        .panes
                        .preview(slot)
                        .and_then(|p| p.shown().cloned())
                        .is_some_and(|s| s == path);
                    let en_vuelo = preview_fetch.get(slot).is_some_and(|f| f.path == path);
                    if !ya && !en_vuelo {
                        // Empezar otra SUSTITUYE la que hubiera: el `Receiver`
                        // viejo se cae aquí y su respuesta no se aplica nunca.
                        preview_fetch.set(slot, Some(spawn_preview_fetch(backend, path)));
                    }
                }
                Some((slot, norte_tui::preview::Want::Note(clave))) => {
                    // Un directorio no se lee: se dice lo que es. Y lo que
                    // hubiera en vuelo deja de importar.
                    preview_fetch.remove(slot);
                    let texto = t(clave);
                    if let Some(p) = app.panes.preview_mut(slot)
                        && (p.note().is_none_or(|n| n != texto) || p.shown().is_some())
                    {
                        p.say(None, texto);
                    }
                }
                None => {}
            }
            // #136: el árbol SÍ pide, y por eso pide UNA rama por vuelta: un
            // directorio de diez mil entradas o un remoto lento no pueden
            // trabar el bucle, y la siguiente vuelta pide la siguiente.
            if let Some(dir) = app.tree().and_then(norte_tui::tree::Tree::wants) {
                let hijos = match backend.list(&dir).await {
                    Ok(mut entries) => {
                        // El MISMO orden que el listado de al lado, con el
                        // mismo comparador: dos columnas que enseñan lo mismo
                        // en distinto orden se leen como si dijeran cosas
                        // distintas.
                        norte_frontend::sort_entries(&mut entries);
                        entries
                            .into_iter()
                            .filter(|e| e.kind == norte_proto::EntryKind::Dir)
                            .map(|e| e.path)
                            .collect()
                    }
                    // Una rama que no se deja leer se marca como leída y VACÍA:
                    // sin esto se volvería a pedir en cada vuelta, que es un
                    // bucle de peticiones contra un directorio prohibido.
                    Err(_) => Vec::new(),
                };
                if let Some(t) = app.tree_mut() {
                    t.insert_children(dir, hijos);
                }
            }
            // La hoja de atributos NO pide nada: lo que enseña ya vino en el
            // listado, así que esto es una copia, no una petición. Un hueco
            // que el reparto no colocó no produce objetivo y no se toca.
            match norte_tui::metadata::want(app, &res) {
                Some((slot, norte_tui::metadata::Want::Entry(e))) => {
                    if let Some(hoja) = app.panes.metadata_mut(slot) {
                        *hoja = Some(*e);
                    }
                }
                Some((slot, norte_tui::metadata::Want::Note(_))) => {
                    if let Some(hoja) = app.panes.metadata_mut(slot) {
                        *hoja = None;
                    }
                }
                None => {}
            }
        }
        // #52: listado lazy — las entradas VISIBLES sin size se hidratan por
        // tandas (máx. una en vuelo; dedup por (pane, path) en `last_probed`).
        if stat_probe.is_none() {
            let tanda: Vec<(usize, VPath)> = app
                .needs_stat_window(STAT_WINDOW_RADIUS)
                .into_iter()
                .filter(|c| !last_probed.contains(c))
                .take(STAT_BATCH_MAX)
                .collect();
            if !tanda.is_empty() {
                last_probed.extend(tanda.iter().cloned());
                stat_probe = Some(spawn_stat_probe(backend, tanda));
            }
        }
        // #157: la fila seleccionada del panel de diferencias, mismo trato.
        if compare_stat_probe.is_none() {
            let objetivos = app.compare_size_probe_targets();
            if !objetivos.is_empty() {
                compare_stat_probe = Some(spawn_compare_stat_probe(
                    backend,
                    objetivos,
                    app.compare_generation(),
                ));
            }
        }
        tokio::select! {
                    _ = session_tick.tick() => {
                        push_session(app, &mut session_push);
                        // #179: soltar el journal cuando lleva un rato sin
                        // usarse. Este proceso lo tomaba en la primera mutación
                        // y no lo devolvía hasta salir, así que una copia a las
                        // 09:00 dejaba a `norte daemon run` y a `norte audit`
                        // sin poder abrirlo en todo el día. La ventana se
                        // reabre sola en la siguiente mutación, y la reapertura
                        // RELEE la cadena, que es lo que lo hace seguro.
                        //
                        // En el CUERPO de la rama y no en su condición: el
                        // cierre NO es cancel-safe, y dropearlo a medias deja
                        // el pool agonizando en el worker de sqlx y al
                        // siguiente `resolve` chocando contra nuestro propio
                        // lock — un aviso de «sesión sin registro» que nos
                        // habríamos inventado nosotros.
                        backend.release_journal_if_idle(JOURNAL_OCIOSO).await;
                    }
                    _ = tick.tick() => {
                        // Mutación terminada → refresh de panes; el ritual completo
                        // (drenador/sonda #52/búsqueda) vive en `after_panes_refresh`
                        // — ÚNICO para los tres disparadores del refresh (#117).
                        let refreshed = on_tick(app, backend, &mut events).await;
                        after_panes_refresh(app, refreshed, &mut fill, &mut last_probed, &mut search_run);
                    }
                    ev = dir_watch.rx.recv(), if dir_watch_alive && watch_refresh_allowed(app) => {
                        // #106: cambio EXTERNO en un dir vigilado (debounced) —
                        // mismo camino que pane.refresh (Ctrl+R): refresh
                        // cancelable + ritual #118. GATEADO (review MAJOR-2): con
                        // un overlay/quick abierto, `refresh_panes` se comería las
                        // teclas del usuario y Esc cambiaría de significado — la
                        // precondición deja el evento ENCOLADO (canal de capacidad
                        // 1) y dispara al cerrarse el overlay.
                        if let Some(()) = ev {
                            let refreshed = refresh_panes(app, backend, &mut events).await;
                            after_panes_refresh(
                                app,
                                refreshed,
                                &mut fill,
                                &mut last_probed,
                                &mut search_run,
                            );
                        } else {
                            // Inalcanzable con `dir_watch` vivo (retiene el emisor
                            // crudo): si pasara, DESARMAR el brazo — un canal
                            // cerrado devolvería None en bucle (spin al 100%,
                            // review MINOR-1).
                            tracing::warn!("dir watch pipeline murió; brazo desarmado");
                            dir_watch_alive = false;
                        }
                    }
                    Some(task) = async {
                        match &mut foreign_tasks {
                            Some(rx) => rx.recv().await,
                            None => std::future::pending().await,
                        }
                    } => {
                        // Task de OTRO frontend de la misma sesión (fase 3): al panel.
                        app.board.push_foreign(&task);
                    }
                    Some(ev) = async {
                        match &mut conn_events {
                            Some(rx) => rx.recv().await,
                            None => std::future::pending().await,
                        }
                    } => {
                        app.message = Some(match ev {
                            ConnEvent::Lost => t("msg-daemon-lost"),
                            ConnEvent::Restored => t("msg-daemon-restored"),
                        });
                    }
                    Some(req) = async {
                        match &mut approvals {
                            Some(rx) => rx.recv().await,
                            None => std::future::pending().await,
                        }
                    } => {
                        // Aprobación de policy pendiente (M3-3b T5): a la cola de
                        // diálogos (jamás pisa un modal abierto) y se abre si procede.
                        app.pending_approvals.push_back(req);
                        app.open_next_pending();
                    }
                    Some(d) = async {
                        match &mut degraded {
                            Some(rx) => rx.recv().await,
                            None => std::future::pending().await,
                        }
                    } => {
                        // #44: sesión remota degradó a texto plano — indicador
                        // PERSISTENTE en la status bar (no pisa `message` transitorio).
                        // H3d: se retiene el valor ESTRUCTURADO, no la frase — la barra
                        // la compone (`App::connection_banner`) y la ayuda puede
                        // preguntar por scheme cuál se degradó.
                        app.note_degraded(d);
                    }
                    Some(estado) = async {
                        match &mut journal_warnings {
                            Some(rx) => rx.recv().await,
                            None => std::future::pending().await,
                        }
                    } => {
                        // #167/#177: esta sesión acaba de mutar sin quedar registrada.
                        // Uno por EPISODIO (el core no repite mientras el motivo no
                        // cambie), así que pisar `message` aquí no puede convertirse en
                        // un goteo. Y como `message` lo borra la siguiente tecla, el
                        // hecho se anota además en el indicador PERSISTENTE de la
                        // barra: esto no es un aviso que se pueda perder por pulsar una
                        // flecha.
                        //
                        // #179: y la recuperación APAGA ese indicador. Sin esto, un
                        // ocupante de paso —otro `norte cp`, un daemon reiniciándose—
                        // dejaría a una sesión de tres horas enseñando «no se registra»
                        // sobre mutaciones que sí se registran.
                        use norte_core::embedded::JournalStatus;
                        match estado {
                            JournalStatus::Lost(why) => {
                                app.message = Some(journal_warning_i18n(&why));
                                app.note_no_journal(why);
                            }
                            JournalStatus::Recovered => {
                                app.message = Some(t("msg-journal-recovered"));
                                app.note_journal_recovered();
                            }
                            // #203: mismo hecho, otra explicación — y la barra lo dice
                            // con otra frase, porque la de siempre sale también cuando
                            // hay un daemon vivo y por eso ya no se mira.
                            JournalStatus::Squatted => {
                                app.message = Some(t("msg-journal-squatted"));
                                app.note_journal_squatted();
                            }
                            // `#[non_exhaustive]`: una transición nueva no puede
                            // cambiar el indicador a ciegas — se ignora hasta que
                            // alguien la enseñe a propósito.
                            _ => {}
                        }
                    }
                    res = async {
                        match &mut stat_probe {
                            Some(pr) => (&mut pr.rx).await.ok(),
                            None => std::future::pending().await,
                        }
                    } => {
                        // Sonda de stat del viewport (#52): el slot se limpia SIEMPRE
                        // (haya hidratado algo, fallara el stat o se cerrara el canal)
                        // — la dedup por `last_probed` evita reintentar hasta que un
                        // listado nuevo la vacíe.
                        stat_probe = None;
                        for (pane, path, entry) in res.unwrap_or_default() {
                            app.panes[pane].hydrate(&path, entry.size, entry.mtime_ms);
                        }
                    }
                    (generation, res) = async {
                        match &mut compare_stat_probe {
                            Some(pr) => (pr.generation, (&mut pr.rx).await.ok()),
                            None => std::future::pending().await,
                        }
                    } => {
                        // Sonda de la fila seleccionada del panel de diferencias
                        // (#157): el slot se limpia SIEMPRE, igual que la de arriba.
                        // Un canal cerrado (`res` es `None`) no marca nada sondeado:
                        // la próxima vez que la selección lo vuelva a pedir se
                        // reintenta, en vez de dejar la fila huérfana para siempre
                        // porque la task que la pedía murió a medio camino.
                        compare_stat_probe = None;
                        for (path, entry) in res.unwrap_or_default() {
                            // La generación es la del PEDIDO, no la de ahora: si otra
                            // comparación empezó mientras volaba, `hydrate` la tira.
                            app.hydrate_compare_size(generation, path, entry.and_then(|e| e.size));
                        }
                    }
                    (slot, res) = std::future::poll_fn(|cx| {
                        // Uno por HUECO, y tantos como huecos haya. `oneshot::Receiver`
                        // es `Unpin`, así que se sondea a mano; `select!` tiene aridad
                        // fija y aquí la aridad la pone el layout.
                        for (id, f) in decorate_fetch.iter_mut() {
                            if let std::task::Poll::Ready(r) =
                                std::pin::Pin::new(&mut f.rx).poll(cx)
                            {
                                return std::task::Poll::Ready((id, r.ok()));
                            }
                        }
                        std::task::Poll::Pending
                    }) => {
                        // Fetch de decoraciones (G3b): se limpia SIEMPRE. Una
                        // respuesta tardía cuyo `dir` ya no case el del pane (el
                        // usuario cd'eó de nuevo mientras estaba en vuelo) se
                        // DESCARTA — nunca pinta badges de un listado que ya no se
                        // ve (mismo criterio anti-stale que el drain-guard de
                        // `apply_fill_msg` para búsqueda virtual).
                        if let Some(f) = decorate_fetch.remove(slot)
                            && let Some((map, cols)) = res
                            && let Some(p) = app.panes.browser_mut(f.slot)
                            && p.dir() == &f.dir
                        {
                            p.set_decorations(map);
                            // #117-follow-up: los valores de columnas plugin:
                            // viajan en el mismo fetch y comparten el guard
                            // anti-stale.
                            p.set_plugin_columns(cols);
                        }
                    }
                    (slot, res) = std::future::poll_fn(|cx| {
                        // Lecturas del preview, una por hueco. Mismo sondeo a mano
                        // que las decoraciones y por el mismo motivo: la aridad la
                        // pone el layout, no `select!`.
                        for (id, f) in preview_fetch.iter_mut() {
                            if let std::task::Poll::Ready(r) =
                                std::pin::Pin::new(&mut f.rx).poll(cx)
                            {
                                return std::task::Poll::Ready((id, r.ok()));
                            }
                        }
                        std::task::Poll::Pending
                    }) => {
                        // La respuesta se aplica SOLO si el hueco sigue queriendo
                        // esa misma ruta: mientras volaba, el cursor pudo moverse.
                        // Y un error se PINTA, jamás se pregunta — el preview sigue
                        // al cursor, así que un diálogo por pulsación convertiría
                        // bajar por un directorio en una ráfaga de modales.
                        if let Some(f) = preview_fetch.remove(slot) {
                            match res {
                                Some(Ok(viewer)) => {
                                    if let Some(p) = app.panes.preview_mut(slot) {
                                        p.show(f.path, viewer);
                                    }
                                }
                                Some(Err(e)) => {
                                    let clave = error_category(&e);
                                    app.preview_failed(slot, &clave);
                                }
                                // La task murió sin contestar: no se pinta un error
                                // inventado, se deja lo que hubiera y el siguiente
                                // movimiento del cursor lo vuelve a intentar.
                                None => {}
                            }
                        }
                    }
                    (pane, msg) = std::future::poll_fn(|cx| {
                        // Un canal POR HUECO, no por posición, y tantos como huecos
                        // haya. `tokio::select!` tiene aridad fija, así que se sondean
                        // a mano: `poll_recv` registra el waker, o sea que esto es tan
                        // cancel-safe como `recv` y perder la carrera no pierde el
                        // lote del otro.
                        //
                        // El barrido ARRANCA donde acabó el anterior. Sondear siempre
                        // desde el principio deja que un drenador rápido en el primer
                        // hueco no deje hablar nunca a los demás — con dos paneles
                        // `select!` lo evitaba solo, porque elige al azar.
                        let n = fill.len();
                        for k in 0..n {
                            let Some((id, f)) = fill.iter_mut().nth((fill_cursor + k) % n) else {
                                break;
                            };
                            if let std::task::Poll::Ready(m) = f.rx.poll_recv(cx) {
                                fill_cursor = (fill_cursor + k + 1) % n;
                                return std::task::Poll::Ready((id, m));
                            }
                        }
                        std::task::Poll::Pending
                    }) => {
                        // Lote del drenador del listado paginado (ADR 0017): al pane
                        // de SU hueco. `None` = canal cerrado (fin del drenado).
                        apply_fill_msg(app, &mut fill, pane, msg);
                    }
                    hits = async {
                        // Solo se drena mientras el run sigue vivo (`Running`): un
                        // canal cerrado devolvería `None` en bucle (spin) — al leer el
                        // `None` se pasa a terminal y este brazo queda pendiente.
                        match &mut search_run {
                            Some(s) if s.state == SearchState::Running => s.rx.recv().await,
                            _ => std::future::pending().await,
                        }
                    } => {
                        drain_search(app, &mut search_run, hits);
                    }
                    batch = async {
                        // Igual que el brazo de hits: solo se drena con el run VIVO,
                        // porque un canal cerrado devolvería `None` en bucle (spin).
                        match &mut compare_run {
                            Some(c) if c.state == CompareState::Running => c.rx.recv().await,
                            _ => std::future::pending().await,
                        }
                    } => {
                        drain_compare(app, &mut compare_run, batch);
                    }
                    // UN solo brazo para las dos Tasks del diálogo: `select!` no deja
                    // tomar prestado `sync_run` dos veces, y son fases sucesivas del
                    // mismo run — nunca hay plan y aplicación a la vez.
                    tick = async {
                        let Some(s) = &mut sync_run else {
                            return std::future::pending().await;
                        };
                        if s.applying {
                            // La aplicación no tiene canal: se espera a que su Task
                            // cambie de estado. `changed()` con el emisor caído
                            // devuelve `Err`, y eso también es un final — se sale y el
                            // cosechado lee el snapshot que haya.
                            let vivo = s.progress.changed().await.is_ok();
                            return SyncTick::Applied { vivo };
                        }
                        match &mut s.rx {
                            // Un canal ya cerrado devolvería `None` en bucle (spin):
                            // `drain_sync_plan` pone `rx = None` al cerrarse.
                            Some(rx) => SyncTick::Plan(rx.recv().await),
                            None => std::future::pending().await,
                        }
                    } => {
                        match tick {
                            SyncTick::Plan(event) => drain_sync_plan(app, &mut sync_run, event),
                            SyncTick::Applied { vivo } => {
                                harvest_sync_apply(app, backend, &mut sync_run, vivo).await;
                            }
                        }
                    }
                    res = async {
                        // ai.rename_plan en vuelo (M4-IA): cosecha sin bloquear —
                        // el brazo solo se arma con un run vivo (molde stat_probe).
                        match &mut ai_rename_run {
                            Some(r) => (&mut r.handle).await,
                            None => std::future::pending().await,
                        }
                    } => {
                        if let Some(run) = ai_rename_run.take() {
                            match res {
                                Ok(Ok(plan)) if plan.entries.is_empty() => {
                                    app.message = Some(t("msg-ai-rename-empty"));
                                }
                                // Cinturón de INGESTIÓN (quality review 78eb243
                                // MINOR-5): un plan legítimo del engine queda muy
                                // por debajo del tope; superarlo delata un daemon
                                // hostil/N+1 inflando la respuesta — rechazo en
                                // bloque, ni se abre el modal.
                                Ok(Ok(plan))
                                    if plan.entries.len() > norte_frontend::MAX_AI_PLAN_ENTRIES =>
                                {
                                    app.message = Some(t("msg-ai-rename-invalid-plan"));
                                }
                                Ok(Ok(plan)) => {
                                    // §17: el plan del LOTE se pide AQUÍ, en el mismo
                                    // viaje que el plan IA — el modal necesita el
                                    // `plan_hash` para que confirmar haga algo, y un
                                    // plan retenido tras otro modal no tendría quién
                                    // se lo pidiera después.
                                    //
                                    // SPAWNEADO, como la llamada al modelo: contra un
                                    // dir enorme o un daemon lento esto es un `fs.list`
                                    // entero, y esperarlo aquí congelaría el loop —
                                    // sin dibujo, sin teclas, sin Esc. El modal abre en
                                    // `Pending` y se rellena solo.
                                    //
                                    // Cinturón fail-loud COMPARTIDO con la GUI (audit
                                    // MAJOR-2): una pareja que no es un `Segment`
                                    // delata un daemon hostil/roto — ni se le pide
                                    // plan al core, y confirmar queda muerto.
                                    let estado = if let Some(pairs) =
                                        norte_frontend::rename_pairs(&plan.entries)
                                    {
                                        let b = backend.clone();
                                        let d = run.dir.clone();
                                        let handle =
                                            tokio::spawn(
                                                async move { b.rename_batch_plan(&d, &pairs).await },
                                            );
                                        if let Some(old) =
                                            rename_batch_run.replace(RenameBatchRun { handle })
                                        {
                                            old.handle.abort();
                                        }
                                        app.message = None;
                                        norte_frontend::BatchPlan::Pending
                                    } else {
                                        app.message = Some(t("msg-ai-rename-invalid-plan"));
                                        norte_frontend::BatchPlan::Failed
                                    };
                                    let ready = PendingAiPlan {
                                        dir: run.dir,
                                        entries: plan.entries,
                                        plan: estado,
                                    };
                                    if app.modal.is_none() {
                                        app.modal = Some(Modal::AiRenamePlan {
                                            dir: ready.dir,
                                            entries: ready.entries,
                                            offset: 0,
                                            plan: ready.plan,
                                        });
                                    } else {
                                        // Otro modal abierto (aprobación, colisión…):
                                        // el plan espera su turno, jamás lo pisa. A
                                        // diferencia de la GUI (banner superseded), aquí
                                        // el overwrite es inalcanzable: run único en
                                        // vuelo y el prompt no abre sobre otro modal.
                                        pending_ai_plan = Some(ready);
                                    }
                                }
                                Ok(Err(e)) => {
                                    app.message = Some(ta(
                                        "msg-ai-rename-failed",
                                        &[("error", &detail_for_bar(&error_category(&e)))],
                                    ));
                                }
                                // Abortado por Esc: silencio, la barra ya se limpió.
                                // (Un pánico del future del backend cae aquí también:
                                // no hay plan que abrir, el run ya está cosechado.)
                                Err(_join) => {}
                            }
                        }
                    }
                    res = async {
                        // fs.rename_batch_plan en vuelo (§17): cosecha sin bloquear,
                        // molde del brazo de `ai_rename_run`.
                        match &mut rename_batch_run {
                            Some(r) => (&mut r.handle).await,
                            None => std::future::pending().await,
                        }
                    } => {
                        if rename_batch_run.take().is_some() {
                            let estado = match res {
                                Ok(Ok(plan)) => norte_frontend::BatchPlan::Ready(Box::new(plan)),
                                Ok(Err(e)) => {
                                    app.message = Some(ta(
                                        "msg-rename-batch-plan-failed",
                                        &[("error", &detail_for_bar(&error_category(&e)))],
                                    ));
                                    norte_frontend::BatchPlan::Failed
                                }
                                // Abortado (otra petición lo relevó) o pánico del
                                // future: no hay plan y no hay nada más que decir —
                                // quien lo relevó ya puso SU mensaje.
                                Err(_join) => norte_frontend::BatchPlan::Failed,
                            };
                            // El modal puede estar abierto, RETENIDO tras otro, o ya
                            // cerrado por el humano. En los dos primeros casos se
                            // rellena; en el tercero la respuesta se tira.
                            if !app.settle_ai_batch_plan(&estado)
                                && let Some(p) = &mut pending_ai_plan
                                && p.plan == norte_frontend::BatchPlan::Pending
                            {
                                p.plan = estado;
                            }
                        }
                    }
                    res = async {
                        // index.search_semantic en vuelo (M4-IA-2): cosecha sin
                        // bloquear — molde del brazo de `ai_rename_run`.
                        match &mut semantic_run {
                            Some(r) => (&mut r.handle).await,
                            None => std::future::pending().await,
                        }
                    } => {
                        semantic_run = None;
                        match res {
                            Ok(Ok(hits)) if hits.is_empty() => {
                                app.message = Some(t("msg-semantic-empty"));
                            }
                            // Cinturón de INGESTIÓN (paridad IA-1): una respuesta
                            // por encima del techo contractual del server o con un
                            // score no finito delata un daemon hostil/N+1 — rechazo
                            // en bloque, ni se abre el modal (el guard es
                            // `norte_frontend::validate_semantic_hits`, pura y
                            // compartida con la GUI).
                            Ok(Ok(hits)) => match norte_frontend::validate_semantic_hits(hits) {
                                None => {
                                    app.message = Some(t("msg-semantic-invalid"));
                                }
                                Some(hits) => {
                                    app.message = None;
                                    if app.modal.is_none() {
                                        app.modal = Some(Modal::SemanticHits {
                                            hits,
                                            offset: 0,
                                            cursor: 0,
                                        });
                                    } else {
                                        // Otro modal abierto (aprobación, colisión…):
                                        // los hits esperan su turno, jamás lo pisan.
                                        pending_semantic = Some(hits);
                                    }
                                }
                            },
                            Ok(Err(e)) => {
                                app.message = Some(ta(
                                    "msg-semantic-failed",
                                    &[("error", &detail_for_bar(&error_category(&e)))],
                                ));
                            }
                            // Abortado por Esc: silencio, la barra ya se limpió.
                            // (Un pánico del future del backend cae aquí también:
                            // no hay hits que abrir, el run ya está cosechado.)
                            Err(_join) => {}
                        }
                    }
                    outcome = async {
                        match &mut lua_run {
                            Some((run, _)) => run.await,
                            None => std::future::pending().await,
                        }
                    } => {
                        // DROP INMEDIATO del CommandRun resuelto (contrato del
                        // driver): retenerlo mantendría `run_active` encendido y la
                        // statusbar Lua congelada.
                        lua_run = None;
                        match outcome {
                            RunOutcome::Ok { messages } => {
                                if !messages.is_empty() {
                                    // Unidos con « · » y por detail_for_bar (tope +
                                    // enmascarado): la barra es una línea.
                                    app.message = Some(detail_for_bar(&messages.join(" · ")));
                                }
                            }
                            RunOutcome::Err { detail, .. } => {
                                app.message = Some(ta(
                                    "err-lua-command",
                                    &[("detail", &detail_for_bar(&detail))],
                                ));
                            }
                            RunOutcome::Cancelled => app.message = Some(t("err-lua-cancelled")),
                            RunOutcome::TimedOut => app.message = Some(t("err-lua-timeout")),
                        }
                        // FIFO: arranca el siguiente encolado. En bucle: si uno ya
                        // no existe (hot-reload lo quitó → `err-lua-unknown`), el
                        // resto de la cola no se queda atascado.
                        if let Some(host) = lua_host.as_ref() {
                            while lua_run.is_none() {
                                let Some(next) = lua_queue.pop_front() else {
                                    break;
                                };
                                lua_run = start_lua_run(app, host, backend, &next);
                            }
                        }
                    }
                    Some(()) = cfg_rx.recv() => {
                        // Ráfaga de guardados: empuja el deadline (ADR 0007).
                        reload_at =
                            Some(tokio::time::Instant::now() + std::time::Duration::from_millis(300));
                    }
                    () = async {
                        match reload_at {
                            Some(d) => tokio::time::sleep_until(d).await,
                            None => std::future::pending().await,
                        }
                    } => {
                        reload_at = None;
                        while cfg_rx.try_recv().is_ok() {}
                        // #117: si el reload cambia los attrs configurados de un
                        // pane visible, hay que re-listar (los valores solo llegan
                        // pidiéndolos) — mismo camino que el confirm del picker.
                        let attrs_before = pane_attr_ids(app);
                        reload_config(
                            app,
                            backend,
                            resolver,
                            viewer_resolver,
                            dialog_resolver,
                            help_lines,
                            lang,
                            &layers,
                            cli_preset.as_deref(),
                            &mut quick_mode,
                            &mut confirm_quit,
                            &mut cfg,
                        )
                        .await;
                        // `[ui] mouse` en caliente: encenderla o apagarla sin
                        // reiniciar. `set` es idempotente, así que un reload que
                        // no tocó la clave (o que falló entero, dejando la config
                        // vigente) no manda nada a la terminal.
                        //
                        // Exención puntual de la regla 2, la MISMA que el draw de
                        // arriba: son unos pocos bytes de escape a la terminal de
                        // control síncronos, acotados, y solo cuando la clave CAMBIA.
                        if let Err(e) =
                            capture.set(cfg.common.ui_mouse.unwrap_or(true), terminal.backend_mut())
                        {
                            // Y se dice, como en el arranque: quien acaba de
                            // encender el ratón desde el overlay de ajustes y se
                            // encuentra con que hacer click no hace nada merece
                            // saber por qué (antes esto solo iba al log).
                            tracing::warn!(error = %e, "no se pudo cambiar la captura de ratón");
                            app.message = Some(t("msg-mouse-capture-failed"));
                        }
                        if pane_attr_ids(app) != attrs_before {
                            let refreshed = refresh_panes(app, backend, &mut events).await;
                            after_panes_refresh(
                                app,
                                refreshed,
                                &mut fill,
                                &mut last_probed,
                                &mut search_run,
                            );
                        }
                        // Hot-reload del scripting Lua (ADR 0026): host NUEVO entero
                        // (jamás estado a medias). Un `CommandRun` en vuelo retiene
                        // el estado VIEJO vía sus handles clonados (documentado en
                        // `lua::api`) y no se toca; statusbar/estado renacen. La
                        // cola también: sus nombres apuntaban al registro viejo
                        // (y si `load_lua` dio None, no quedaría quién drenarla).
                        lua_host = load_lua(app, &layers).await;
                        lua_queue.clear();
                    }
                    maybe = events.next() => {
                        // EOF del terminal —te cierran la ventana—: se sale por
                        // el MISMO sitio que un `app.quit`, que es donde se
                        // guarda la última foto de la sesión. Saliendo aquí con
                        // un `return` se perdía.
                        let Some(event) = maybe else { app.quit = true; continue; };
                        let event = event.map_err(RunError::Event)?;
                        // Ratón (`[ui] mouse`): solo llega si la captura está
                        // pedida — sin ella el emulador no reporta nada y este
                        // brazo no corre. La semántica del gesto (marcar, barrer,
                        // transferir) vive en `norte-frontend` (regla 7); aquí solo
                        // se resuelve la celda y se aplica.
                        if let Event::Mouse(me) = event {
                            match mouse::handle(app, me) {
                                mouse::After::Nothing => {}
                                // Pulsar un elemento del menú: el ratón ya
                                // dejó el cursor encima; ejecutarlo es
                                // asíncrono y necesita el backend, así que se
                                // remata aquí — el MISMO camino que `Enter`,
                                // que es lo que hace que un menú y una tecla no
                                // puedan divergir.
                                mouse::After::MenuAccept => {
                                    let elegido = app
                                        .menu
                                        .as_ref()
                                        .and_then(norte_frontend::menu::MenuState::selected);
                                    app.menu = None;
                                    if let Some(id) = elegido
                                        && let Some(cmd) = Command::parse(id)
                                    {
                                        let outcome = dispatch(
                                            app,
                                            backend,
                                            &mut events,
                                            help_lines,
                                            lang,
                                            quick_mode,
                                            confirm_quit,
                                            &cfg,
                                            cmd,
                                        )
                                        .await;
                                        apply_cd(
                                            &app.panes,
                                            &mut fill,
                                            &mut decorate_fetch,
                                            &mut last_probed,
                                            &mut search_run,
                                            outcome,
                                        );
                                        reap_search_run(app, &mut search_run);
                                        if let Some(pending) = app.pending_open.take() {
                                            app.message = Some(
                                                launch_opener(terminal, capture, pending).await,
                                            );
                                        }
                                    }
                                }
                                // Doble click = `nav.enter`, por el MISMO `dispatch`
                                // que la tecla: mismo cd, mismo relleno paginado,
                                // misma cosecha de la búsqueda viva. Un segundo
                                // camino para entrar en un directorio sería un
                                // segundo sitio donde arreglar cada bug de cd.
                                mouse::After::Enter => {
                                    // K3a: un gesto es OTRA entrada. La secuencia que
                                    // el lector estuviera tecleando se abandona con su
                                    // panel — no la completa el ratón, y dejarla
                                    // armada haría que la siguiente tecla disparase un
                                    // comando pedido antes de cambiar de directorio.
                                    app.abandon_pending(resolver);
                                    let outcome = dispatch(
                                        app,
                                        backend,
                                        &mut events,
                                        help_lines,
                                        lang,
                                        quick_mode,
                                        confirm_quit,
                                        &cfg,
                                        Command::NavEnter,
                                    )
                                    .await;
                                    if let Some(pane) = cd_landed_pane(&outcome) {
                                        app.apply_scheme_sort(pane);
                                        let dir = app.panes[pane].dir().clone();
                                        let paths: Vec<VPath> = app.panes[pane]
                                            .entries()
                                            .iter()
                                            .map(|e| e.path.clone())
                                            .collect();
                                        let plugin_cols = app.columns.plugin_ids_for(dir.scheme());
                                        decorate_fetch.set(
            app.panes.slot_of(pane),
            spawn_decorate_fetch(backend, app.panes.slot_of(pane), dir, paths, plugin_cols),
        );
                                    }
                                    apply_cd(
                                        &app.panes,
                                        &mut fill,
                                        &mut decorate_fetch,
                                        &mut last_probed,
                                        &mut search_run,
                                        outcome,
                                    );
                                    // Paridad con el sitio del resolver: entrar en
                                    // un hit apaga el modo virtual del pane, y hay
                                    // que cosechar el run (regla 3).
                                    reap_search_run(app, &mut search_run);
                                }
                                // #226: el sidebar con el ratón toma los MISMOS
                                // caminos que su teclado. Desplegar las unidades
                                // es el momento de volver a pedirlas —y plegarlas,
                                // el de no pedirlas—, así que el ratón no puede
                                // ser un cuarto disparador de refresco: es este.
                                mouse::After::PlacesFolded => {
                                    if app.places_drives_visible() {
                                        refresh_places_drives(app, backend).await;
                                    }
                                }
                                // Y activar una fila lleva el listado por el
                                // flujo de `cd` de siempre, igual que `Enter`
                                // dentro del sidebar.
                                mouse::After::PlacesActivate => {
                                    app.abandon_pending(resolver);
                                    if let Some(path) = app.places_activate() {
                                        let pane = app.focus();
                                        let outcome =
                                            cd_in(app, backend, &mut events, pane, path, Trail::Record)
                                                .await;
                                        apply_cd(
                                            &app.panes,
                                            &mut fill,
                                            &mut decorate_fetch,
                                            &mut last_probed,
                                            &mut search_run,
                                            outcome,
                                        );
                                    }
                                }
                            }
                        } else if let Event::Key(key) = event
                            && key.kind == crossterm::event::KeyEventKind::Press
                        {
                            app.message = None;
                            if app.menu.is_some() && !modal_wins(app) {
                                // La barra de menús: teclas FIJAS, como la
                                // palette. No hay verbos `dialog.*` para
                                // «siguiente menú», así que tampoco pueden
                                // salir del keymap.
                                let plain = key.modifiers.is_empty()
                                    || key.modifiers == KeyModifiers::SHIFT;
                                match key.code {
                                    KeyCode::Esc if plain => app.menu = None,
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
                                        let elegido = app
                                            .menu
                                            .as_ref()
                                            .and_then(norte_frontend::menu::MenuState::selected);
                                        app.menu = None;
                                        if let Some(id) = elegido
                                            && let Some(cmd) = Command::parse(id)
                                        {
                                            // MISMO camino que la palette y que
                                            // el resolver: un comando elegido en
                                            // un menú corre exactamente como si
                                            // se hubiera pulsado su tecla.
                                            //
                                            // El cuerpo está duplicado del brazo
                                            // de la palette a sabiendas:
                                            // extraerlo pide una función de doce
                                            // parámetros —`&mut events`,
                                            // `terminal`, `capture`— o refactorizar
                                            // el run loop, y ninguna de las dos
                                            // cabe en el cambio que trae el menú.
                                            let outcome = dispatch(
                                                app,
                                                backend,
                                                &mut events,
                                                help_lines,
                                                lang,
                                                quick_mode,
                                                confirm_quit,
                                                &cfg,
                                                cmd,
                                            )
                                            .await;
                                            if let Some(pane) = cd_landed_pane(&outcome) {
                                                app.apply_scheme_sort(pane);
                                                let dir = app.panes[pane].dir().clone();
                                                let paths: Vec<VPath> = app.panes[pane]
                                                    .entries()
                                                    .iter()
                                                    .map(|e| e.path.clone())
                                                    .collect();
                                                let plugin_cols =
                                                    app.columns.plugin_ids_for(dir.scheme());
                                                decorate_fetch.set(
                                                    app.panes.slot_of(pane),
                                                    spawn_decorate_fetch(
                                                        backend,
                                                        app.panes.slot_of(pane),
                                                        dir,
                                                        paths,
                                                        plugin_cols,
                                                    ),
                                                );
                                            }
                                            apply_cd(
                                                &app.panes,
                                                &mut fill,
                                                &mut decorate_fetch,
                                                &mut last_probed,
                                                &mut search_run,
                                                outcome,
                                            );
                                            reap_search_run(app, &mut search_run);
                                            if let Some(pending) = app.pending_open.take() {
                                                app.message = Some(
                                                    launch_opener(terminal, capture, pending).await,
                                                );
                                            }
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
                                if let Some(url) =
                                    on_connections_picker_key(app, dialog_resolver, key.modifiers, key.code)
                                {
                                    match VPath::parse(&url) {
                                        Ok(destino) => {
                                            let outcome = cd(app, backend, &mut events, destino).await;
                                            apply_cd(
                                                &app.panes,
                                                &mut fill,
                                                &mut decorate_fetch,
                                                &mut last_probed,
                                                &mut search_run,
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
                            } else if app.columns_picker.is_some() && !modal_wins(app) {
                                // Picker de columnas (#108 7a): mismo puesto en la
                                // cadena que el selector de tema (overlay antes que
                                // el brazo del modal, precedencia existente).
                                if on_columns_key(app, dialog_resolver, key.modifiers, key.code).await {
                                    // #117: el set de attrs pintado cambió — los
                                    // valores solo llegan pidiéndolos, así que se
                                    // re-lista por el MISMO camino que tras una
                                    // mutación (ritual en `after_panes_refresh`).
                                    let refreshed = refresh_panes(app, backend, &mut events).await;
                                    after_panes_refresh(
                                        app,
                                        refreshed,
                                        &mut fill,
                                        &mut last_probed,
                                        &mut search_run,
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
                            } else if app.key_owner() == norte_tui::app::KeyOwner::Tree
                                && !modal_wins(app)
                            {
                                // #136: el árbol manda el listado a la rama
                                // elegida por el flujo de cd de siempre.
                                let outcome = on_tree_key(
                                    app,
                                    backend,
                                    &mut events,
                                    dialog_resolver,
                                    key.modifiers,
                                    key.code,
                                )
                                .await;
                                apply_cd(
                                    &app.panes,
                                    &mut fill,
                                    &mut decorate_fetch,
                                    &mut last_probed,
                                    &mut search_run,
                                    outcome,
                                );
                            } else if app.key_owner() == norte_tui::app::KeyOwner::Places
                                && !modal_wins(app)
                            {
                                // Sidebar de sitios (L3): Enter sobre una fila
                                // manda el LISTADO enfocado a ese sitio, por el
                                // flujo de cd de siempre.
                                let outcome = on_places_key(
                                    app,
                                    backend,
                                    &mut events,
                                    dialog_resolver,
                                    key.modifiers,
                                    key.code,
                                )
                                .await;
                                if let Some(pane) = cd_landed_pane(&outcome) {
                                    app.apply_scheme_sort(pane);
                                    let dir = app.panes[pane].dir().clone();
                                    let paths: Vec<VPath> =
                                        app.panes[pane].entries().iter().map(|e| e.path.clone()).collect();
                                    let plugin_cols = app.columns.plugin_ids_for(dir.scheme());
                                    decorate_fetch.set(
            app.panes.slot_of(pane),
            spawn_decorate_fetch(backend, app.panes.slot_of(pane), dir, paths, plugin_cols),
        );
                                }
                                apply_cd(
                                    &app.panes,
                                    &mut fill,
                                    &mut decorate_fetch,
                                    &mut last_probed,
                                    &mut search_run,
                                    outcome,
                                );
                            } else if app.key_owner() == norte_tui::app::KeyOwner::Processes
                                && !modal_wins(app)
                            {
                                // Panel de procesos (#243): las teclas llegan
                                // AQUÍ, y no al listado de detrás. Sin este
                                // brazo el panel cogía el borde de foco, el
                                // `▶` no se movía nunca y F8 abría el diálogo
                                // de borrar sobre la selección del listado
                                // mientras el lector creía tener el teclado en
                                // la lista de tareas.
                                on_processes_key(app, dialog_resolver, key.modifiers, key.code);
                            } else if app.nav_popup.is_some() && !modal_wins(app) {
                                // Popup historial/hotlist (spec 2026-07-18): Enter
                                // sobre un item NAVEGA por el flujo de cd normal —
                                // su desenlace toca el relleno como cualquier cd.
                                let outcome = on_nav_popup_key(
                                    app,
                                    backend,
                                    &mut events,
                                    dialog_resolver,
                                    key.modifiers,
                                    key.code,
                                )
                                .await;
                                if let Some(pane) = cd_landed_pane(&outcome) {
                                    app.apply_scheme_sort(pane);
                                    let dir = app.panes[pane].dir().clone();
                                    let paths: Vec<VPath> =
                                        app.panes[pane].entries().iter().map(|e| e.path.clone()).collect();
                                    let plugin_cols = app.columns.plugin_ids_for(dir.scheme());
                                    decorate_fetch.set(
            app.panes.slot_of(pane),
            spawn_decorate_fetch(backend, app.panes.slot_of(pane), dir, paths, plugin_cols),
        );
                                }
                                apply_cd(
                                    &app.panes,
                                    &mut fill,
                                    &mut decorate_fetch,
                                    &mut last_probed,
                                    &mut search_run,
                                    outcome,
                                );
                            } else if app.search_dialog.is_some() && !modal_wins(app) {
                                // Diálogo Alt+F7 (liveSearch T6): captura imprimibles
                                // como los demás overlays; Enter con criterio lanza la
                                // búsqueda (abre el pane virtual) — el resto de teclas
                                // no navegan.
                                if let Some(params) =
                                    on_search_dialog_key(app, key.modifiers, key.code)
                                {
                                    launch_search(app, backend, &mut fill, &mut search_run, params)
                                        .await;
                                }
                            } else if app.sync.is_some() && !modal_wins(app) {
                                // Panel de sincronización: teclas FIJAS, como las del
                                // de diferencias. Va ANTES que él porque se pinta
                                // encima: el de diferencias sigue vivo detrás con sus
                                // marcas, y el teclado tiene que ir a lo que se ve.
                                on_sync_key(app, &mut sync_run, key.modifiers, key.code);
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
                                    &mut events,
                                    &mut fill,
                                    &mut decorate_fetch,
                                    &mut last_probed,
                                    &mut search_run,
                                    &mut compare_run,
                                    key.modifiers,
                                    key.code,
                                )
                                .await;
                            } else if app.palette.is_some() && !modal_wins(app) {
                                // Command palette (H1 T4): editor de filtro libre,
                                // como el diálogo de búsqueda de arriba — sus
                                // teclas son FIJAS, no resuelven por el contexto
                                // `dialog` (decisión 8 del plan H1: no hay
                                // vocabulario `dialog.*` para "teclear un carácter"
                                // o "correr la selección"). ctrl+c conserva su
                                // significado global (salir), como TODOS los
                                // overlays.
                                if key.modifiers.contains(KeyModifiers::CONTROL)
                                    && key.code == KeyCode::Char('c')
                                {
                                    app.quit = true;
                                    continue;
                                }
                                let plain = key.modifiers.is_empty()
                                    || key.modifiers == KeyModifiers::SHIFT;
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
                                            // (P1) Enter sobre una fila de PLUGIN: la
                                            // `key` es `plugin:{id}:{command}`
                                            // (`palette::plugin_rows`, jamás pintada)
                                            // — no vive en `COMMANDS`, así que se
                                            // enruta AQUÍ, antes del vocabulario
                                            // tipado (#112). El resultado del plugin
                                            // es texto NO confiable: `detail_for_bar`
                                            // (enmascarado + tope, patrón #73).
                                            if let Some((id, command)) = parse_plugin_key(&cmd) {
                                                let (id, command) =
                                                    (id.to_owned(), command.to_owned());
                                                run_plugin_command(app, backend, &id, &command)
                                                    .await;
                                                continue;
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
                                                continue;
                                            };
                                            let outcome = dispatch(
                                                app,
                                                backend,
                                                &mut events,
                                                help_lines,
                                                lang,
                                                quick_mode,
                                                confirm_quit,
                                                &cfg,
                                                cmd,
                                            )
                                            .await;
                                            if let Some(pane) = cd_landed_pane(&outcome) {
                                    app.apply_scheme_sort(pane);
                                    let dir = app.panes[pane].dir().clone();
                                    let paths: Vec<VPath> =
                                        app.panes[pane].entries().iter().map(|e| e.path.clone()).collect();
                                    let plugin_cols = app.columns.plugin_ids_for(dir.scheme());
                                    decorate_fetch.set(
            app.panes.slot_of(pane),
            spawn_decorate_fetch(backend, app.panes.slot_of(pane), dir, paths, plugin_cols),
        );
                                }
                                apply_cd(
                                    &app.panes,
                                    &mut fill,
                                    &mut decorate_fetch,
                                    &mut last_probed,
                                    &mut search_run,
                                    outcome,
                                );
                                            // Paridad con el sitio del resolver (#118
                                            // review): un cd elegido en la palette
                                            // (nav.parent…) también puede apagar el
                                            // modo virtual — cosecha del run (regla 3);
                                            // y un `pane.open` de la palette deja su
                                            // comando externo resuelto — lanzarlo YA,
                                            // no en la siguiente tecla.
                                            reap_search_run(app, &mut search_run);
                                            if let Some(pending) = app.pending_open.take() {
                                                app.message = Some(launch_opener(terminal, capture, pending).await);
                                            }
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
                                    &cfg,
                                    cli_preset.as_deref(),
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
                                    let outcome = dispatch(
                                        app,
                                        backend,
                                        &mut events,
                                        help_lines,
                                        lang,
                                        quick_mode,
                                        confirm_quit,
                                        &cfg,
                                        cmd,
                                    )
                                    .await;
                                    if let Some(pane) = cd_landed_pane(&outcome) {
                                        app.apply_scheme_sort(pane);
                                        let dir = app.panes[pane].dir().clone();
                                        let paths: Vec<VPath> = app.panes[pane]
                                            .entries()
                                            .iter()
                                            .map(|e| e.path.clone())
                                            .collect();
                                        let plugin_cols = app.columns.plugin_ids_for(dir.scheme());
                                        decorate_fetch.set(
            app.panes.slot_of(pane),
            spawn_decorate_fetch(backend, app.panes.slot_of(pane), dir, paths, plugin_cols),
        );
                                    }
                                    apply_cd(
                                        &app.panes,
                                        &mut fill,
                                        &mut decorate_fetch,
                                        &mut last_probed,
                                        &mut search_run,
                                        outcome,
                                    );
                                    reap_search_run(app, &mut search_run);
                                    if let Some(pending) = app.pending_open.take() {
                                        app.message =
                                            Some(launch_opener(terminal, capture, pending).await);
                                    }
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
                                // que vive en este loop): no navega ni toca `fill`.
                                if matches!(app.modal, Some(Modal::TrustLuaInit { .. })) {
                                    resolve_lua_trust(app, lua_host.as_ref(), key.code).await;
                                    continue;
                                }
                                // ctrl+c conserva su significado global (salir),
                                // como los demás overlays (H1 T2) — ANTES de
                                // resolver contra el contexto `dialog`, hardcodeado.
                                if key.modifiers.contains(KeyModifiers::CONTROL)
                                    && key.code == KeyCode::Char('c')
                                {
                                    app.quit = true;
                                    continue;
                                }
                                // `Modal::MarkPattern` (#103 T9) es TEXTO libre, como
                                // el diálogo de búsqueda de arriba: consume
                                // caracteres crudos ANTES del contexto `dialog` — no
                                // tiene ALLOWLIST de `dialog_action` (`ctrl+c` ya
                                // quedó resuelto arriba, igual que para el resto de
                                // modales).
                                if matches!(app.modal, Some(Modal::MarkPattern { .. })) {
                                    let plain = key.modifiers.is_empty()
                                        || key.modifiers == KeyModifiers::SHIFT;
                                    match key.code {
                                        KeyCode::Char(c) if plain => app.mark_pattern_push(c),
                                        KeyCode::Backspace if plain => app.mark_pattern_pop(),
                                        // Un `Err` deja el diagnóstico en el propio
                                        // modal (`mark_pattern_confirm`, que lo deja
                                        // abierto): nada más que hacer aquí.
                                        KeyCode::Enter if plain => {
                                            if let Ok(n) = app.mark_pattern_confirm() {
                                                app.message = Some(ta(
                                                    "msg-marked-by-pattern",
                                                    &[("n", &n.to_string())],
                                                ));
                                            }
                                        }
                                        KeyCode::Esc if plain => app.cancel_mark_pattern(),
                                        _ => {}
                                    }
                                    continue;
                                }
                                // `Modal::Mkdir` (#104): mismo molde de texto libre.
                                // El submit vive AQUÍ (async): el modal valida y
                                // devuelve el destino; la task se registra en el
                                // board como cualquier otra mutación.
                                if matches!(app.modal, Some(Modal::Mkdir { .. })) {
                                    let plain = key.modifiers.is_empty()
                                        || key.modifiers == KeyModifiers::SHIFT;
                                    match key.code {
                                        KeyCode::Char(c) if plain => app.mkdir_push(c),
                                        KeyCode::Backspace if plain => app.mkdir_pop(),
                                        KeyCode::Enter if plain => {
                                            if let Some(target) = app.mkdir_confirm() {
                                                match backend.mkdir(&target).await {
                                                    Ok(task) => {
                                                        app.board.push(&task, None);
                                                        app.mkdir_submitted();
                                                    }
                                                    // MINOR-1: el nombre sobrevive
                                                    // al fallo del submit.
                                                    Err(e) => app
                                                        .mkdir_set_error(error_message(&e)),
                                                }
                                            }
                                        }
                                        KeyCode::Esc if plain => app.cancel_mkdir(),
                                        _ => {}
                                    }
                                    continue;
                                }
                                // #132: empaquetar. Mismo molde de texto
                                // libre que Mkdir, y el submit AQUÍ por lo
                                // mismo: es async y la task va al board.
                                if matches!(app.modal, Some(Modal::Pack { .. })) {
                                    let plain = key.modifiers.is_empty()
                                        || key.modifiers == KeyModifiers::SHIFT;
                                    match key.code {
                                        KeyCode::Char(c) if plain => app.pack_push(c),
                                        KeyCode::Backspace if plain => app.pack_pop(),
                                        KeyCode::Enter if plain => {
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
                                        KeyCode::Esc if plain => app.cancel_pack(),
                                        _ => {}
                                    }
                                    continue;
                                }
                                // #132: partir. Igual.
                                if matches!(app.modal, Some(Modal::Split { .. })) {
                                    let plain = key.modifiers.is_empty()
                                        || key.modifiers == KeyModifiers::SHIFT;
                                    match key.code {
                                        KeyCode::Char(c) if plain => app.split_push(c),
                                        KeyCode::Backspace if plain => app.split_pop(),
                                        KeyCode::Enter if plain => {
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
                                        KeyCode::Esc if plain => app.cancel_split(),
                                        _ => {}
                                    }
                                    continue;
                                }
                                // `Modal::TransferDest`: mismo molde de texto libre.
                                // Enter no transfiere — abre el modal de siempre
                                // (`open_transfer_to_dir`), que es donde vive la
                                // confirmación; un destino que no parsea deja su
                                // diagnóstico y conserva lo tecleado.
                                if matches!(app.modal, Some(Modal::TransferDest { .. })) {
                                    let plain = key.modifiers.is_empty()
                                        || key.modifiers == KeyModifiers::SHIFT;
                                    match key.code {
                                        KeyCode::Char(c) if plain => app.transfer_dest_push(c),
                                        KeyCode::Backspace if plain => app.transfer_dest_pop(),
                                        KeyCode::Enter if plain => {
                                            let _ = app.transfer_dest_confirm();
                                        }
                                        KeyCode::Esc if plain => app.cancel_transfer_dest(),
                                        _ => {}
                                    }
                                    continue;
                                }
                                // `Modal::CommandLine` (#135): mismo molde de texto
                                // libre que Mkdir. Enter deja la SUSPENSIÓN pendiente
                                // (la ejecuta la cabecera de la vuelta, que es donde
                                // vive la terminal) y cierra el prompt.
                                if matches!(app.modal, Some(Modal::CommandLine { .. })) {
                                    let plain = key.modifiers.is_empty()
                                        || key.modifiers == KeyModifiers::SHIFT;
                                    match key.code {
                                        KeyCode::Char(c) if plain => app.command_line_push(c),
                                        KeyCode::Backspace if plain => app.command_line_pop(),
                                        KeyCode::Enter if plain => {
                                            if let Some(cmd) = app.command_line_confirm() {
                                                submit_command_line(app, &cmd);
                                            }
                                        }
                                        KeyCode::Esc if plain => app.cancel_command_line(),
                                        _ => {}
                                    }
                                    continue;
                                }
                                // `Modal::AiRenameInstruction` (M4-IA): mismo molde de
                                // texto libre que Mkdir. Enter SPAWNEA la petición al
                                // modelo (la única llamada larga del loop) y cierra el
                                // prompt; la cosecha vive en el select.
                                if matches!(app.modal, Some(Modal::AiRenameInstruction { .. })) {
                                    let plain = key.modifiers.is_empty()
                                        || key.modifiers == KeyModifiers::SHIFT;
                                    match key.code {
                                        KeyCode::Char(c) if plain => app.ai_rename_push(c),
                                        KeyCode::Backspace if plain => app.ai_rename_pop(),
                                        KeyCode::Enter if plain => {
                                            if let Some(instruction) = app.ai_rename_confirm() {
                                                let dir = app.focused().dir().clone();
                                                let b = backend.clone();
                                                let d = dir.clone();
                                                let handle = tokio::spawn(async move {
                                                    b.ai_rename_plan(&d, &instruction).await
                                                });
                                                // Relanzar con un run vivo lo ABORTA
                                                // (dropear el handle solo desvincula):
                                                // a lo sumo una petición en vuelo.
                                                if let Some(old) =
                                                    ai_rename_run.replace(AiRenameRun { handle, dir })
                                                {
                                                    old.handle.abort();
                                                }
                                                // Invariante: lanzar VACÍA el stash —
                                                // un plan retenido de una petición
                                                // ANTERIOR jamás debe abrirse como si
                                                // fuera de esta.
                                                pending_ai_plan = None;
                                                app.message = Some(t("msg-ai-rename-running"));
                                                app.ai_rename_submitted();
                                            }
                                        }
                                        KeyCode::Esc if plain => app.cancel_ai_rename(),
                                        _ => {}
                                    }
                                    continue;
                                }
                                // `Modal::SemanticQuery` (M4-IA-2): mismo molde de
                                // texto libre. Enter SPAWNEA la consulta al índice
                                // (root = None: todos los roots) y cierra el prompt;
                                // la cosecha vive en el select.
                                if matches!(app.modal, Some(Modal::SemanticQuery { .. })) {
                                    let plain = key.modifiers.is_empty()
                                        || key.modifiers == KeyModifiers::SHIFT;
                                    match key.code {
                                        KeyCode::Char(c) if plain => app.semantic_push(c),
                                        KeyCode::Backspace if plain => app.semantic_pop(),
                                        KeyCode::Enter if plain => {
                                            if let Some(query) = app.semantic_confirm() {
                                                let b = backend.clone();
                                                let handle = tokio::spawn(async move {
                                                    b.index_search_semantic(None, &query, SEMANTIC_K)
                                                        .await
                                                });
                                                // Relanzar con un run vivo lo ABORTA
                                                // (dropear el handle solo desvincula):
                                                // a lo sumo una consulta en vuelo.
                                                if let Some(old) =
                                                    semantic_run.replace(SemanticRun { handle })
                                                {
                                                    old.handle.abort();
                                                }
                                                // Invariante: lanzar VACÍA el stash —
                                                // unos hits retenidos de una consulta
                                                // ANTERIOR jamás deben abrirse como si
                                                // fueran de esta.
                                                pending_semantic = None;
                                                app.message = Some(t("msg-semantic-running"));
                                                app.semantic_submitted();
                                            }
                                        }
                                        KeyCode::Esc if plain => app.cancel_semantic(),
                                        _ => {}
                                    }
                                    continue;
                                }
                                // `Modal::TransferName` (#105): mismo molde. El
                                // submit reusa `submit_transfer` — colisiones por el
                                // camino existente (`Modal::Collision` + backlog).
                                if matches!(app.modal, Some(Modal::TransferName { .. })) {
                                    let plain = key.modifiers.is_empty()
                                        || key.modifiers == KeyModifiers::SHIFT;
                                    match key.code {
                                        KeyCode::Char(c) if plain => app.transfer_name_push(c),
                                        KeyCode::Backspace if plain => app.transfer_name_pop(),
                                        KeyCode::Enter if plain => {
                                            if let Some((kind, from, dest)) =
                                                app.transfer_name_confirm()
                                            {
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
                                                    app.transfer_name_set_error(t(
                                                        "msg-transfer-name-failed",
                                                    ));
                                                }
                                            }
                                        }
                                        KeyCode::Esc if plain => app.cancel_transfer_name(),
                                        _ => {}
                                    }
                                    continue;
                                }
                                // El modal TOFU (#45) puede NAVEGAR al confiar: su Cd
                                // se aplica igual que el de un comando.
                                let outcome = on_dialog_key(
                                    app,
                                    backend,
                                    &mut events,
                                    dialog_resolver,
                                    key.modifiers,
                                    key.code,
                                    lang,
                                    help_lines,
                                )
                                .await;
                                if let Some(pane) = cd_landed_pane(&outcome) {
                                    app.apply_scheme_sort(pane);
                                    let dir = app.panes[pane].dir().clone();
                                    let paths: Vec<VPath> =
                                        app.panes[pane].entries().iter().map(|e| e.path.clone()).collect();
                                    let plugin_cols = app.columns.plugin_ids_for(dir.scheme());
                                    decorate_fetch.set(
            app.panes.slot_of(pane),
            spawn_decorate_fetch(backend, app.panes.slot_of(pane), dir, paths, plugin_cols),
        );
                                }
                                apply_cd(
                                    &app.panes,
                                    &mut fill,
                                    &mut decorate_fetch,
                                    &mut last_probed,
                                    &mut search_run,
                                    outcome,
                                );
                            } else {
                                // Esc con un comando Lua en vuelo (BROWSE: sin modal
                                // ni overlay, y NO en el viewer): pide cancelación
                                // (regla 3) y CONSUME la tecla — no cae al resolver.
                                if app.viewer.is_none()
                                    && key.modifiers.is_empty()
                                    && key.code == KeyCode::Esc
                                    && let Some((_, token)) = &lua_run
                                {
                                    token.cancel();
                                    // K3a: la tecla se CONSUME aquí, así que el
                                    // resolver no la ve — y una secuencia a medias
                                    // (con su panel which-key encima) se quedaría
                                    // armada mientras el lector cree haber cancelado.
                                    app.abandon_pending(resolver);
                                    continue;
                                }
                                // Esc con ai.rename_plan en vuelo (BROWSE, M4-IA):
                                // cancelar (regla 3) y CONSUMIR la tecla. Abortar
                                // dropea el future del backend en el runtime →
                                // rpc.cancel (remoto) / drop del stream (embebido).
                                if app.viewer.is_none()
                                    && key.modifiers.is_empty()
                                    && key.code == KeyCode::Esc
                                    && let Some(run) = ai_rename_run.take()
                                {
                                    run.handle.abort();
                                    app.message = None;
                                    // K3a: ídem — Esc consumido aquí también cancela
                                    // la secuencia en vuelo, jamás solo su pintura.
                                    app.abandon_pending(resolver);
                                    continue;
                                }
                                // Esc con una búsqueda semántica en vuelo (BROWSE,
                                // M4-IA-2): mismo contrato de cancelación (regla 3).
                                if app.viewer.is_none()
                                    && key.modifiers.is_empty()
                                    && key.code == KeyCode::Esc
                                    && let Some(run) = semantic_run.take()
                                {
                                    run.handle.abort();
                                    app.message = None;
                                    app.abandon_pending(resolver);
                                    continue;
                                }
                                // Pane virtual de búsqueda (liveSearch T6): con un
                                // search_run en el pane con foco (y sin quick vivo),
                                // Esc y Enter tienen semántica propia ANTES del
                                // resolver. El RESTO de teclas (cursor, F5/F8/F3…) cae
                                // al resolver y opera sobre el hit bajo el cursor.
                                if app.viewer.is_none()
                                    && key.modifiers.is_empty()
                                    && app.focused().quick().is_none()
                                    && app.focused().virtual_search
                                    && search_run
                                        .as_ref()
                                        .is_some_and(|s| s.pane == app.focus())
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
                                                &mut events,
                                                &mut fill,
                                                &mut decorate_fetch,
                                                &mut last_probed,
                                                &mut search_run,
                                            )
                                            .await;
                                            continue;
                                        }
                                        KeyCode::Enter => {
                                            // Enter sobre un hit: cd al PADRE del hit y
                                            // cursor sobre él (sale del modo virtual).
                                            on_search_enter(
                                                app,
                                                backend,
                                                &mut events,
                                                &mut fill,
                                                &mut decorate_fetch,
                                                &mut last_probed,
                                                &mut search_run,
                                            )
                                            .await;
                                            continue;
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
                                    let plain = key.modifiers.is_empty()
                                        || key.modifiers == KeyModifiers::SHIFT;
                                    match key.code {
                                        KeyCode::Char(c) if plain => {
                                            app.focused_mut().quick_char(c);
                                            continue;
                                        }
                                        KeyCode::Backspace if plain => {
                                            app.focused_mut().quick_backspace();
                                            continue;
                                        }
                                        KeyCode::Up if plain => {
                                            app.focused_mut().quick_up();
                                            continue;
                                        }
                                        KeyCode::Down if plain => {
                                            app.focused_mut().quick_down();
                                            continue;
                                        }
                                        KeyCode::Tab if plain && jump => {
                                            app.focused_mut().quick_next();
                                            continue;
                                        }
                                        KeyCode::Esc if plain => {
                                            app.focused_mut().quick_cancel();
                                            continue;
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
                                                let outcome = dispatch(
                                                    app,
                                                    backend,
                                                    &mut events,
                                                    help_lines,
                                                    lang,
                                                    quick_mode,
                                                    confirm_quit,
                                                    &cfg,
                                                    Command::NavEnter,
                                                )
                                                .await;
                                                if let Some(pane) = cd_landed_pane(&outcome) {
                                    app.apply_scheme_sort(pane);
                                    let dir = app.panes[pane].dir().clone();
                                    let paths: Vec<VPath> =
                                        app.panes[pane].entries().iter().map(|e| e.path.clone()).collect();
                                    let plugin_cols = app.columns.plugin_ids_for(dir.scheme());
                                    decorate_fetch.set(
            app.panes.slot_of(pane),
            spawn_decorate_fetch(backend, app.panes.slot_of(pane), dir, paths, plugin_cols),
        );
                                }
                                apply_cd(
                                    &app.panes,
                                    &mut fill,
                                    &mut decorate_fetch,
                                    &mut last_probed,
                                    &mut search_run,
                                    outcome,
                                );
                                                // Paridad con el sitio del resolver
                                                // (#118 review): el Enter del quick
                                                // search ES un nav.enter — entrar en
                                                // un hit apaga el modo virtual del
                                                // pane; sin cosecha, la Task de
                                                // búsqueda quedaba viva (regla 3).
                                                reap_search_run(app, &mut search_run);
                                            }
                                            continue;
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
                                    let ctrl_enter = key.code == KeyCode::Enter
                                        && key.modifiers == KeyModifiers::CONTROL;
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
                                            &mut events,
                                            help_lines,
                                            lang,
                                            quick_mode,
                                            confirm_quit,
                                            &cfg,
                                            Command::AppPickAccept,
                                        )
                                        .await;
                                        continue;
                                    }
                                }
                                // Pantalla activa: el viewer tiene su contexto.
                                let active = if app.viewer.is_some()
                                    || app.key_owner() == norte_tui::app::KeyOwner::Preview
                                {
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
                                        Resolution::Run { command: cmd, count } => {
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
                                                    lua_host.as_ref(),
                                                    backend,
                                                    name,
                                                    &mut lua_run,
                                                    &mut lua_queue,
                                                );
                                                continue;
                                            }
                                            // #112: el keymap se validó contra
                                            // COMMANDS al cargar — el parse no puede
                                            // fallar; guard defensivo.
                                            let Some(cmd) = Command::parse(&cmd) else {
                                                debug_assert!(false, "keymap fuera de COMMANDS");
                                                continue;
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
                                                    &mut events,
                                                    help_lines,
                                                    lang,
                                                    quick_mode,
                                                    confirm_quit,
                                                    &cfg,
                                                    cmd,
                                                )
                                                .await;
                                                // Leído ANTES de que `apply_cd`
                                                // consuma el outcome.
                                                let stalled = nav_stalled(cmd, &outcome);
                                                if let Some(pane) = cd_landed_pane(&outcome) {
                                                    app.apply_scheme_sort(pane);
                                                    let dir = app.panes[pane].dir().clone();
                                                    let paths: Vec<VPath> = app.panes[pane]
                                                        .entries()
                                                        .iter()
                                                        .map(|e| e.path.clone())
                                                        .collect();
                                                    let plugin_cols =
                                                        app.columns.plugin_ids_for(dir.scheme());
                                                    decorate_fetch.set(
                                                        app.panes.slot_of(pane),
                                                        spawn_decorate_fetch(
                                                            backend,
                                                            app.panes.slot_of(pane),
                                                            dir,
                                                            paths,
                                                            plugin_cols,
                                                        ),
                                                    );
                                                }
                                                apply_cd(
                                                    &app.panes,
                                                    &mut fill,
                                                    &mut decorate_fetch,
                                                    &mut last_probed,
                                                    &mut search_run,
                                                    outcome,
                                                );
                                                // Un cd (nav.parent…) apagó el modo
                                                // virtual del pane de búsqueda: suelta
                                                // el run y cancela.
                                                reap_search_run(app, &mut search_run);
                                                // #28: `pane.open` dejó un comando
                                                // externo resuelto — el run loop (dueño
                                                // de la terminal) sondea el binario y
                                                // lo lanza.
                                                if let Some(pending) = app.pending_open.take() {
                                                    app.message = Some(
                                                        launch_opener(terminal, capture, pending).await,
                                                    );
                                                }
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
                                                if app.quit
                                                    || stalled
                                                    || keyboard_owner(app) != owner_before
                                                {
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
                        } else if let Event::Paste(text) = event {
                            // Bracketed paste (#143): ONE router beside the key
                            // dispatch above, not a second one — see `route_paste`.
                            app.message = None;
                            route_paste(app, &text);
                        }
                    }
                }
        // Resize/Focus/etc: el draw del inicio del loop repinta solo.
    }
}

/// Dirs NATIVOS vigilables de los panes (#106): solo `file://` (un dir
/// sftp/S3/archive no tiene inotify — su refresh sigue siendo Ctrl+R) y
/// solo panes reales (el virtual de búsqueda no muestra un dir). Puro:
/// `vpath_to_native` no toca el FS.
fn watch_targets(app: &App) -> [Option<std::path::PathBuf>; 2] {
    std::array::from_fn(|i| {
        let p = &app.panes[i];
        if p.virtual_search {
            return None;
        }
        norte_vfs_local::vpath_to_native(p.dir()).ok()
    })
}

/// La OTRA decisión que `walk_trail` depende de y nadie pinchaba: qué
/// navegaciones entran en el rastro. El guard `trail == Trail::Record` es la
/// única línea que impide que `nav.back` se alimente de su propio rastro.
#[cfg(test)]
mod record_step_tests {
    use super::{Trail, TrailStep, nav, record_step};
    use norte_proto::VPath;

    fn vp(wire: &str) -> VPath {
        VPath::parse(wire).expect("wire de test")
    }

    /// Una navegación del USUARIO deja huella en las dos estructuras: el
    /// rastro que recorre `nav.back` y la MRU que pinta el popup.
    #[test]
    fn una_navegacion_del_usuario_entra_en_el_rastro_y_en_la_mru() {
        let mut h = nav::History::default();
        record_step(&mut h, &vp("mem:///a"), &vp("mem:///b"), Trail::Record);
        assert_eq!(h.back_len(), 1, "un paso en el rastro");
        assert!(h.entries().contains(&vp("mem:///a")), "y en la MRU");
    }

    /// EL guard. Un `Replay` es el rastro recorriéndose a sí mismo: si
    /// registrara, volver de B a A grabaría «estuve en B», el siguiente atrás
    /// devolvería a B, y el lector oscilaría entre dos directorios para
    /// siempre. Borra `&& trail == Trail::Record` de `record_step` y este
    /// test se pone rojo — es su único guardián.
    #[test]
    fn un_replay_no_alimenta_el_rastro() {
        let mut h = nav::History::default();
        record_step(
            &mut h,
            &vp("mem:///b"),
            &vp("mem:///a"),
            Trail::Replay(TrailStep::Back),
        );
        assert_eq!(h.back_len(), 0, "un paso atrás jamás produce rastro");
        assert!(
            h.entries().is_empty(),
            "ni entra en la MRU: volver no es visitar un sitio nuevo"
        );
    }

    /// Un cd al MISMO dir (refresh-like) no es un paso que el lector diera:
    /// registrarlo haría que el siguiente `nav.back` no hiciera nada visible.
    #[test]
    fn un_cd_al_mismo_dir_no_es_un_paso() {
        let mut h = nav::History::default();
        record_step(&mut h, &vp("mem:///a"), &vp("mem:///a"), Trail::Record);
        assert_eq!(h.back_len(), 0);
        assert!(h.entries().is_empty());
    }
}

#[cfg(test)]
mod help_freeze_tests {
    use super::{App, Pane, open_contextual_help};
    use norte_help::ChordResolver as _;
    use norte_proto::VPath;

    /// Abrir la ayuda CONGELA los hechos del contexto (H3d).
    ///
    /// El resto de la cadena —la tabla compartida, el resolver, el pintor de la
    /// razón— tiene sus propios tests y seguiría VERDE con esta llamada
    /// borrada: el overlay se pintaría contra el resolver permisivo del
    /// arranque y ninguna fila se atenuaría jamás. Este test es el único que
    /// mira el eslabón.
    ///
    /// Se abre desde dentro de un zip (`READ_ONLY` por el scheme, ADR 0018) con
    /// los dos panes ahí: sin destino escribible, `pane.copy` no puede correr.
    #[test]
    fn abrir_la_ayuda_congela_los_hechos_del_contexto() {
        let dentro = VPath::parse("zip+file:///a.zip/!").expect("wire de test");
        let mut app = App::new(
            Pane::new(dentro.clone(), Vec::new()),
            Pane::new(dentro, Vec::new()),
        );
        assert!(
            app.help_chords.availability("pane.copy").is_available(),
            "antes de abrir, el resolver del arranque no atenúa nada"
        );

        open_contextual_help(&mut app, norte_help::Lang::En, &[], None);

        assert!(app.help.is_some(), "el overlay se abrió");
        assert_eq!(
            app.help_chords.availability("pane.copy").reason(),
            Some(norte_help::Reason::ReadOnlyBackend),
            "la ayuda tiene que saber que está dentro de un archivo"
        );
    }
}

#[cfg(test)]
mod caps_cache_tests {
    use super::{App, Pane, cache_capabilities, needs_capabilities};
    use norte_proto::VPath;

    fn vp(wire: &str) -> VPath {
        VPath::parse(wire).expect("wire de test")
    }

    fn app_en(dir: &VPath) -> App {
        App::new(
            Pane::new(dir.clone(), Vec::new()),
            Pane::new(dir.clone(), Vec::new()),
        )
    }

    /// H3d: `fs.capabilities` devuelve catálogo Y flags en una respuesta, y
    /// las dos mitades se cachean. La que se tiraba era la de los flags, y
    /// tirarla costaba una ronda de red extra la próxima vez que alguien
    /// preguntase si el pane era de solo lectura.
    #[test]
    fn se_cachean_las_dos_mitades_de_una_respuesta() {
        let dir = vp("mem:///");
        let mut app = app_en(&dir);
        let caps = norte_proto::Capabilities {
            flags: norte_proto::CapabilityFlags::READ_ONLY,
            max_path: None,
        };
        let catalog = norte_proto::AttrCatalog::new(Vec::new());

        cache_capabilities(&mut app, &dir, (caps, catalog));

        assert_eq!(app.caps(&dir), Some(&caps), "los flags se quedaron");
        assert!(
            app.attr_catalog("mem").is_some(),
            "y el catálogo, que es la mitad que ya se guardaba"
        );
        // Y el efecto que la ayuda consume: con el flag puesto, el pane es de
        // solo lectura sin volver a preguntar a nadie.
        assert!(app.pane_read_only(0));
    }

    /// MAJOR-1, la otra mitad: la puerta que decide si se pregunta tiene que
    /// preguntar lo MISMO que responde el caché. Gateada solo por el catálogo
    /// —que es por scheme—, un `cd` a un segundo host de `sftp` no volvía a
    /// llamar jamás, así que las caps del primero contestaban por él durante
    /// toda la sesión.
    #[test]
    fn otra_authority_del_mismo_scheme_vuelve_a_preguntar() {
        let a = vp("sftp://a.org/");
        let b = vp("sftp://b.org/");
        let mut app = app_en(&a);
        assert!(needs_capabilities(&app, &a), "sin nada cacheado, se pide");

        cache_capabilities(
            &mut app,
            &a,
            (
                norte_proto::Capabilities {
                    flags: norte_proto::CapabilityFlags::READ_ONLY,
                    max_path: None,
                },
                norte_proto::AttrCatalog::new(Vec::new()),
            ),
        );

        assert!(
            !needs_capabilities(&app, &a),
            "al mismo host no se le pregunta dos veces"
        );
        assert!(
            needs_capabilities(&app, &b),
            "b.org no ha contestado nunca: hay que preguntarle a ÉL"
        );
    }

    /// #215: otro DIRECTORIO del mismo backend vuelve a preguntar.
    ///
    /// Desde ADR 0054 el daemon contesta por UBICACIÓN, y la caché seguía
    /// indexando por conexión: bajo un mismo `file://` hay montajes —un pincho
    /// exFAT que pliega caja, un subárbol ext4 en `+F`, un bind de solo
    /// lectura— y la respuesta de `/home` se servía para todos ellos.
    #[test]
    fn otro_directorio_del_mismo_backend_vuelve_a_preguntar() {
        let casa = vp("file:///home/yo");
        let pincho = vp("file:///media/pincho");
        let mut app = app_en(&casa);

        cache_capabilities(
            &mut app,
            &casa,
            (
                norte_proto::Capabilities {
                    flags: norte_proto::CapabilityFlags::empty(),
                    max_path: None,
                },
                norte_proto::AttrCatalog::new(Vec::new()),
            ),
        );

        assert!(!needs_capabilities(&app, &casa));
        assert!(
            needs_capabilities(&app, &pincho),
            "un montaje distinto contesta por su cuenta"
        );
        assert!(
            app.caps(&pincho).is_none(),
            "y hasta que conteste, no hay respuesta suya que servir"
        );
    }

    /// La caché tiene TOPE: una clave por directorio ya no está acotada por
    /// los siete schemes que existen, y recorrer un árbol grande la haría
    /// crecer sin fin. Se desaloja el más viejo por orden de llegada.
    #[test]
    fn la_cache_de_capacidades_tiene_tope() {
        let primero = vp("file:///d0");
        let mut app = app_en(&primero);
        let caps = norte_proto::Capabilities {
            flags: norte_proto::CapabilityFlags::empty(),
            max_path: None,
        };
        for i in 0..200 {
            app.insert_caps(&vp(&format!("file:///d{i}")), caps);
        }
        assert!(
            app.caps(&primero).is_none(),
            "el primero se fue al llenarse"
        );
        assert!(app.caps(&vp("file:///d199")).is_some(), "y el último sigue");
    }

    /// La costura entera, contra un backend REAL: `first_page` con la puerta
    /// abierta trae las caps y el `cd` las guarda.
    ///
    /// Sin esto, el cableado podía revertirse en silencio y la suite quedaba
    /// verde: `App::pane_read_only` cae al criterio SINTÁCTICO del scheme
    /// cuando no hay caps, y hoy los dos coinciden en todo provider que
    /// existe. Ninguna otra prueba distingue «llegaron los flags» de «el
    /// scheme lo parecía».
    #[tokio::test]
    async fn la_primera_pagina_trae_las_caps_y_el_cd_las_guarda() {
        use norte_core::backend::Backend;
        use std::sync::Arc;

        let engine = norte_core::Engine::new();
        engine.register_provider(Arc::new(norte_testkit::MemProvider::new()));
        let backend = Backend::Embedded(Arc::new(engine));
        let dir = vp("mem:///");

        let (_first, _stream, _skipped, both) = super::first_page(&backend, &dir, &[], true)
            .await
            .expect("el listado del provider de memoria");
        let both = both.expect("con la puerta abierta llegan las DOS mitades");

        let mut app = app_en(&dir);
        assert!(app.caps(&dir).is_none());
        cache_capabilities(&mut app, &dir, both);
        assert!(
            app.caps(&dir).is_some(),
            "las caps de la respuesta tienen que quedarse en el caché"
        );

        // Y con la puerta CERRADA no se pregunta: el cuarto elemento es None.
        let (_f, _s, _k, ninguna) = super::first_page(&backend, &dir, &[], false)
            .await
            .expect("el listado igual");
        assert!(
            ninguna.is_none(),
            "con la puerta cerrada no hay ronda extra"
        );
    }
}

#[cfg(test)]
mod mirror_fill_tests {
    use super::{
        App, Cd, DecorateFetch, Fill, FillMsg, Pane, Probed, SearchRun, apply_cd, apply_fill_msg,
    };
    use norte_proto::{Entry, EntryKind, Segment, VPath};

    fn vp(w: &str) -> VPath {
        VPath::parse(w).expect("wire de test")
    }

    fn file(dir: &VPath, name: &str) -> Entry {
        Entry {
            attrs: std::collections::BTreeMap::new(),
            path: dir.join(Segment::new(name.as_bytes().to_vec()).unwrap()),
            kind: EntryKind::File,
            size: Some(1),
            mtime_ms: None,
        }
    }

    /// Un relleno paginado por PANE, y no uno global: `pane.mirror` manda el
    /// OTRO pane a un sitio SIN mover el foco, así que con un solo hueco basta
    /// una tecla para que el pane que el lector está mirando —el suyo, el
    /// enfocado, aún paginando un dir grande— se quede a medias.
    ///
    /// Soltar su `rx` mata al drenador sin `finish_listing`, y `loading` solo
    /// lo apaga `finish_listing`/`Failed`/un listado nuevo: el pane queda con
    /// el listado truncado bajo un «cargando…» permanente.
    #[test]
    fn un_espejo_al_otro_pane_no_estrangula_el_relleno_del_pane_mirado() {
        let dir = vp("file:///d");
        let mut app = App::new(
            Pane::new(dir.clone(), Vec::new()),
            Pane::new(dir.clone(), Vec::new()),
        );
        // El pane 0 —el enfocado, el que el lector mira— está paginando.
        app.panes[0].begin_listing(dir.clone(), vec![file(&dir, "a")], true, None);
        let (tx0, rx0) = tokio::sync::mpsc::channel::<FillMsg>(1);
        let mut fill: norte_frontend::layout::BySlot<Fill> = norte_frontend::layout::BySlot::new();
        fill.insert(norte_tui::panel::SLOT_LEFT, Fill { rx: rx0 });
        let mut df: norte_frontend::layout::BySlot<DecorateFetch> =
            norte_frontend::layout::BySlot::new();
        let mut lp = Probed::new();

        // `pane.mirror`: el pane 1 viaja, y su listado también viene paginado.
        let (_tx1, rx1) = tokio::sync::mpsc::channel::<FillMsg>(1);
        app.panes[1].begin_listing(dir.clone(), Vec::new(), true, None);
        let mut sr: Option<SearchRun> = None;
        apply_cd(
            &app.panes,
            &mut fill,
            &mut df,
            &mut lp,
            &mut sr,
            Cd::Filling {
                pane: 1,
                fill: Fill { rx: rx1 },
            },
        );

        // El drenador del pane 0 sigue teniendo a quién enviar: nadie le
        // soltó el `rx` por debajo.
        tx0.try_send(FillMsg::Batch(vec![file(&dir, "b")]))
            .expect("el drenador del pane 0 no fue abandonado");
        let msg = fill
            .get_mut(norte_tui::panel::SLOT_LEFT)
            .expect("el relleno del pane 0 sigue en su hueco")
            .rx
            .try_recv()
            .ok();
        apply_fill_msg(&mut app, &mut fill, norte_tui::panel::SLOT_LEFT, msg);

        assert_eq!(
            app.panes[0].entries().len(),
            2,
            "el lote posterior entra en el listado del pane 0"
        );
        assert!(
            app.panes[0].loading(),
            "y el «cargando…» sigue vivo: nadie terminó el listado por él"
        );
        assert!(
            fill.get(norte_tui::panel::SLOT_RIGHT).is_some(),
            "el espejo se quedó con SU hueco"
        );
    }
}

#[cfg(test)]
mod apply_cd_tests {
    use super::{Cd, DecorateFetch, Fill, FillMsg, Probed, SearchRun, apply_cd};
    use norte_frontend::layout::BySlot;
    use norte_tui::panel::{PaneSlots, SLOT_LEFT, SLOT_RIGHT};

    /// Dos paneles de mentira: `apply_cd` solo les pregunta qué hueco ocupa
    /// cada posición.
    fn panes() -> PaneSlots {
        let d = norte_proto::VPath::parse("mem:///x").expect("wire");
        PaneSlots::new(
            norte_tui::app::Pane::new(d.clone(), Vec::new()),
            norte_tui::app::Pane::new(d, Vec::new()),
        )
    }

    fn hueco(i: usize) -> norte_frontend::layout::SlotId {
        if i == 0 { SLOT_LEFT } else { SLOT_RIGHT }
    }
    use norte_proto::Error;

    fn fill() -> Fill {
        let (_tx, rx) = tokio::sync::mpsc::channel::<FillMsg>(1);
        Fill { rx }
    }

    /// El hueco del pane 0 ocupado y el del 1 libre: la disposición de
    /// partida de casi todos estos casos.
    fn en_el_pane_0() -> BySlot<Fill> {
        let mut f = BySlot::new();
        f.insert(SLOT_LEFT, fill());
        f
    }

    /// Un REEMPLAZO del mismo pane suelta su relleno obsoleto.
    #[test]
    fn replaced_suelta_el_fill_del_pane() {
        let mut f = en_el_pane_0();
        let mut lp = Probed::new();
        let mut df: BySlot<DecorateFetch> = BySlot::new();
        let mut sr: Option<SearchRun> = None;
        apply_cd(&panes(), &mut f, &mut df, &mut lp, &mut sr, Cd::Replaced(0));
        assert!(
            f.get(hueco(0)).is_none(),
            "el fill del listado viejo se suelta"
        );
    }

    /// Un reemplazo de OTRO pane no toca el relleno vivo.
    #[test]
    fn replaced_de_otro_pane_no_toca() {
        let mut f = en_el_pane_0();
        let mut lp = Probed::new();
        let mut df: BySlot<DecorateFetch> = BySlot::new();
        let mut sr: Option<SearchRun> = None;
        apply_cd(&panes(), &mut f, &mut df, &mut lp, &mut sr, Cd::Replaced(1));
        assert!(f.get(hueco(0)).is_some(), "el fill del pane 0 sobrevive");
    }

    /// #78: un cd FALLIDO NO suelta el relleno — el pane sigue en su listado
    /// anterior, que se sigue rellenando (soltarlo lo colgaba en loading).
    #[test]
    fn failed_conserva_el_fill() {
        let mut f = en_el_pane_0();
        let mut lp = Probed::new();
        let mut df: BySlot<DecorateFetch> = BySlot::new();
        let mut sr: Option<SearchRun> = None;
        apply_cd(
            &panes(),
            &mut f,
            &mut df,
            &mut lp,
            &mut sr,
            Cd::Failed(Error::NotFound),
        );
        assert!(
            f.get(hueco(0)).is_some(),
            "el fill del listado anterior sigue vivo tras un cd fallido"
        );
    }

    /// Un cd abandonado no toca nada.
    #[test]
    fn cancelled_conserva_el_fill() {
        let mut f = en_el_pane_0();
        let mut lp = Probed::new();
        let mut df: BySlot<DecorateFetch> = BySlot::new();
        let mut sr: Option<SearchRun> = None;
        apply_cd(&panes(), &mut f, &mut df, &mut lp, &mut sr, Cd::Cancelled);
        assert!(f.get(hueco(0)).is_some());
    }

    /// Un listado nuevo ocupa el hueco DE SU PANE y solo ese: el relleno del
    /// otro pane sigue drenando. Es lo que hace que `pane.mirror` —que manda
    /// el OTRO pane a un sitio sin mover el foco— no pueda dejar a medias el
    /// pane que el lector está mirando.
    #[test]
    fn filling_de_un_pane_no_toca_el_hueco_del_otro() {
        let mut f = en_el_pane_0();
        let mut lp = Probed::new();
        let mut df: BySlot<DecorateFetch> = BySlot::new();
        let mut sr: Option<SearchRun> = None;
        apply_cd(
            &panes(),
            &mut f,
            &mut df,
            &mut lp,
            &mut sr,
            Cd::Filling {
                pane: 1,
                fill: fill(),
            },
        );
        assert!(
            f.get(hueco(0)).is_some(),
            "el relleno del pane 0 sigue en su hueco"
        );
        assert!(f.get(hueco(1)).is_some(), "y el nuevo ocupa el suyo");
    }

    /// #118: Ctrl+R re-listó el pane 0 (listado COMPLETO nuevo) — su
    /// drenador viejo duplicaría filas si siguiera vivo. La dedup de la
    /// sonda #52 también caduca: el listado nuevo re-lazifica las entries.
    #[test]
    fn refreshed_suelta_el_fill_del_pane_relistado() {
        let mut f = en_el_pane_0();
        let mut lp = Probed::from([(0, norte_proto::VPath::parse("file:///d/x").unwrap())]);
        let mut df: BySlot<DecorateFetch> = BySlot::new();
        let mut sr: Option<SearchRun> = None;
        apply_cd(
            &panes(),
            &mut f,
            &mut df,
            &mut lp,
            &mut sr,
            Cd::Refreshed([true, false]),
        );
        assert!(
            f.get(hueco(0)).is_none(),
            "el drenador del listado viejo se suelta"
        );
        assert!(lp.is_empty(), "la dedup de la sonda #52 caduca");
    }

    /// #118: Esc a medias — el pane 1 NO llegó a re-listarse, su relleno
    /// paginado sigue siendo válido (#78: soltarlo lo colgaba en loading).
    #[test]
    fn refreshed_a_medias_conserva_el_fill_del_pane_no_relistado() {
        let mut f: norte_frontend::layout::BySlot<Fill> = norte_frontend::layout::BySlot::new();
        f.insert(norte_tui::panel::SLOT_RIGHT, fill());
        let mut lp = Probed::new();
        let mut df: BySlot<DecorateFetch> = BySlot::new();
        let mut sr: Option<SearchRun> = None;
        apply_cd(
            &panes(),
            &mut f,
            &mut df,
            &mut lp,
            &mut sr,
            Cd::Refreshed([true, false]),
        );
        assert!(
            f.get(hueco(1)).is_some(),
            "el fill del pane NO re-listado sobrevive al Esc a medias"
        );
    }

    /// Y el simétrico: un refresh de los DOS panes suelta los dos huecos.
    #[test]
    fn refreshed_de_ambos_panes_suelta_los_dos_huecos() {
        let mut f: norte_frontend::layout::BySlot<Fill> = norte_frontend::layout::BySlot::new();
        f.insert(SLOT_LEFT, fill());
        f.insert(SLOT_RIGHT, fill());
        let mut lp = Probed::new();
        let mut df: BySlot<DecorateFetch> = BySlot::new();
        let mut sr: Option<SearchRun> = None;
        apply_cd(
            &panes(),
            &mut f,
            &mut df,
            &mut lp,
            &mut sr,
            Cd::Refreshed([true, true]),
        );
        assert!(f.get(hueco(0)).is_none() && f.get(hueco(1)).is_none());
    }

    /// #118: refresh totalmente abandonado (Esc antes del primer pane) o
    /// ambos panes en modo virtual: nada cambió, nada se toca.
    #[test]
    fn refreshed_vacio_no_toca_nada() {
        let mut f = en_el_pane_0();
        let mut lp = Probed::from([(0, norte_proto::VPath::parse("file:///d/x").unwrap())]);
        let mut df: BySlot<DecorateFetch> = BySlot::new();
        let mut sr: Option<SearchRun> = None;
        apply_cd(
            &panes(),
            &mut f,
            &mut df,
            &mut lp,
            &mut sr,
            Cd::Refreshed([false, false]),
        );
        assert!(
            f.get(hueco(0)).is_some(),
            "sin pane re-listado, el fill sigue"
        );
        assert!(!lp.is_empty(), "sin pane re-listado, la dedup sigue");
    }
}

#[cfg(test)]
mod swap_tests {
    use super::{
        App, Cd, DecorateFetch, Fill, FillMsg, Pane, Probed, SearchRun, SearchState, apply_cd,
        reap_search_run, reconcile_swap, watch_targets,
    };
    use norte_core::backend::TaskRef;
    use norte_proto::VPath;
    use norte_proto::methods::SearchHits;

    fn vp(wire: &str) -> VPath {
        VPath::parse(wire).expect("wire de test")
    }

    /// Una búsqueda viva CORRIENDO sobre `pane`. La Task es sintética (el
    /// `TaskRef` de test del core): aquí no se ejerce el walker, sino el
    /// índice de pane que el run loop guarda a su lado.
    fn search_run(pane: usize) -> SearchRun {
        let (_tx, rx) = tokio::sync::mpsc::channel::<SearchHits>(1);
        let id = norte_proto::TaskId::new(1);
        let (_progreso, prx) = tokio::sync::watch::channel(norte_proto::TaskProgress {
            task_id: id,
            kind: norte_proto::TaskKind::Search,
            state: norte_proto::TaskState::Running,
            bytes_done: 0,
            bytes_total: None,
            entries_done: 0,
            entries_total: None,
            current: None,
        });
        SearchRun {
            task: TaskRef::synthetic_for_tests(id, prx),
            rx,
            pane,
            prev_dir: vp("file:///antes"),
            hits: 0,
            state: SearchState::Running,
        }
    }

    fn decorate(slot: norte_frontend::layout::SlotId) -> DecorateFetch {
        let (_tx, rx) = tokio::sync::oneshot::channel();
        DecorateFetch {
            slot,
            dir: vp("mem:///d"),
            rx,
        }
    }

    /// El relleno EN VUELO está archivado POR PANE: si el intercambio no cruza
    /// los huecos, los lotes del listado siguen llegando al pane de al lado y
    /// el lector ve crecer la lista equivocada. Es el bug que una suite verde
    /// no ve, porque el listado sigue llegando: solo llega al sitio que no es.
    ///
    /// Comprueba que se movió ESE drenador y no un hueco cualquiera: el lote
    /// enviado por el `tx` del pane 0 se recoge del hueco del pane 1.
    #[test]
    fn el_intercambio_cruza_los_huecos_del_relleno_en_vuelo() {
        let (tx, rx) = tokio::sync::mpsc::channel::<FillMsg>(1);
        let mut f: norte_frontend::layout::BySlot<Fill> = norte_frontend::layout::BySlot::new();
        f.insert(norte_tui::panel::SLOT_LEFT, Fill { rx });
        let mut df: norte_frontend::layout::BySlot<DecorateFetch> =
            norte_frontend::layout::BySlot::new();
        df.insert(
            norte_tui::panel::SLOT_LEFT,
            decorate(norte_tui::panel::SLOT_LEFT),
        );
        let mut lp = Probed::from([(0, vp("mem:///d/x"))]);
        let mut sr: Option<SearchRun> = None;

        reconcile_swap(
            norte_tui::panel::SLOT_LEFT,
            norte_tui::panel::SLOT_RIGHT,
            &mut f,
            &mut df,
            &mut lp,
            &mut sr,
        );

        assert!(
            f.get(norte_tui::panel::SLOT_LEFT).is_none(),
            "el hueco del pane 0 queda libre"
        );
        tx.try_send(FillMsg::Failed)
            .expect("el drenador sigue vivo");
        assert!(
            f.get_mut(norte_tui::panel::SLOT_RIGHT)
                .expect("cruzado al hueco del pane 1")
                .rx
                .try_recv()
                .is_ok(),
            "y es EL MISMO drenador el que ahora alimenta al pane 1"
        );
        assert!(
            df.get(norte_tui::panel::SLOT_RIGHT).is_some()
                && df.get(norte_tui::panel::SLOT_LEFT).is_none(),
            "cruzados"
        );
        assert!(lp.is_empty(), "la caché de stat se tira, no se traduce");
    }

    /// Sin nada en vuelo el reconciliado es inofensivo: un intercambio no
    /// puede inventar un relleno ni un fetch donde no los había.
    #[test]
    fn el_intercambio_sin_nada_en_vuelo_no_inventa_nada() {
        let mut f: norte_frontend::layout::BySlot<Fill> = norte_frontend::layout::BySlot::new();
        let mut df: norte_frontend::layout::BySlot<DecorateFetch> =
            norte_frontend::layout::BySlot::new();
        let mut lp = Probed::new();
        let mut sr: Option<SearchRun> = None;
        reconcile_swap(
            norte_tui::panel::SLOT_LEFT,
            norte_tui::panel::SLOT_RIGHT,
            &mut f,
            &mut df,
            &mut lp,
            &mut sr,
        );
        assert!(
            f.get(norte_tui::panel::SLOT_LEFT).is_none()
                && f.get(norte_tui::panel::SLOT_RIGHT).is_none()
        );
        assert!(
            df.get(norte_tui::panel::SLOT_LEFT).is_none()
                && df.get(norte_tui::panel::SLOT_RIGHT).is_none()
        );
    }

    /// El desenlace `Cd::Swapped` tiene que LLEGAR al reconciliado: la mitad
    /// del intercambio que `dispatch` no puede hacer viaja por `apply_cd`, y
    /// un brazo que se olvidara de llamarlo dejaría el fill apuntando al pane
    /// que no es sin que ningún test de `App` se enterase.
    #[test]
    fn apply_cd_swapped_reconcilia_el_estado_del_run_loop() {
        let (_tx, rx) = tokio::sync::mpsc::channel::<FillMsg>(1);
        let mut f: norte_frontend::layout::BySlot<Fill> = norte_frontend::layout::BySlot::new();
        f.insert(norte_tui::panel::SLOT_RIGHT, Fill { rx });
        let mut df: norte_frontend::layout::BySlot<DecorateFetch> =
            norte_frontend::layout::BySlot::new();
        df.insert(
            norte_tui::panel::SLOT_RIGHT,
            decorate(norte_tui::panel::SLOT_RIGHT),
        );
        let mut lp = Probed::from([(1, vp("mem:///d/x"))]);
        let mut sr: Option<SearchRun> = None;
        let panes = norte_tui::panel::PaneSlots::new(
            Pane::new(vp("mem:///d"), Vec::new()),
            Pane::new(vp("mem:///d"), Vec::new()),
        );
        apply_cd(&panes, &mut f, &mut df, &mut lp, &mut sr, Cd::Swapped);
        assert!(
            f.get(norte_tui::panel::SLOT_LEFT).is_some()
                && f.get(norte_tui::panel::SLOT_RIGHT).is_none()
        );
        assert!(
            df.get(norte_tui::panel::SLOT_LEFT).is_some()
                && df.get(norte_tui::panel::SLOT_RIGHT).is_none()
        );
        assert!(lp.is_empty());
    }

    /// La búsqueda VIVA también está indexada por pane: `SearchRun` guarda el
    /// pane virtual que muestra los hits, exactamente como el relleno guarda
    /// el suyo. Si el intercambio no lo voltea, los hits siguen entrando en el
    /// pane de al lado.
    #[test]
    fn el_intercambio_voltea_el_pane_de_la_busqueda_viva() {
        let mut f: norte_frontend::layout::BySlot<Fill> = norte_frontend::layout::BySlot::new();
        let mut df: norte_frontend::layout::BySlot<DecorateFetch> =
            norte_frontend::layout::BySlot::new();
        let mut lp = Probed::new();
        let mut sr = Some(search_run(0));

        reconcile_swap(
            norte_tui::panel::SLOT_LEFT,
            norte_tui::panel::SLOT_RIGHT,
            &mut f,
            &mut df,
            &mut lp,
            &mut sr,
        );

        assert_eq!(
            sr.as_ref().expect("el run sigue vivo").pane,
            1,
            "el pane virtual de la búsqueda cambió de lado con su pane"
        );
    }

    /// Y el intercambio no puede COSECHAR la búsqueda por el camino.
    ///
    /// El `Esc`/`Enter` del pane virtual son las ÚNICAS teclas que ese modo
    /// intercepta, así que un `Ctrl+U` cae al resolutor y cruza los panes con
    /// una búsqueda corriendo. Justo después, el mismo call site pasa por
    /// [`reap_search_run`], que suelta el run cuando su pane ya no es virtual:
    /// con el `pane` sin voltear mira el pane 0 —que ahora tiene el listado
    /// ordinario que vino del otro lado— y CANCELA la Task en silencio,
    /// dejando el pane 1 con hits a medias en `Running` para siempre y sin su
    /// manejador de `Esc` (que exige un run vivo PARA ESE pane).
    ///
    /// Por eso el volteo tiene que ocurrir DENTRO de `reconcile_swap`: pasada
    /// la cosecha ya no hay nada que salvar.
    #[test]
    fn un_intercambio_no_cosecha_la_busqueda_viva() {
        let mut app = App::new(
            Pane::new(vp("file:///izq"), Vec::new()),
            Pane::new(vp("file:///der"), Vec::new()),
        );
        // Búsqueda viva en el pane 0 (el `Alt+F7` lo dejó virtual).
        app.panes[0].begin_search(vp("file:///izq"));
        let mut sr = Some(search_run(0));
        let mut f: norte_frontend::layout::BySlot<Fill> = norte_frontend::layout::BySlot::new();
        let mut df: norte_frontend::layout::BySlot<DecorateFetch> =
            norte_frontend::layout::BySlot::new();
        let mut lp = Probed::new();

        // `Ctrl+U`: `dispatch` cruza los panes y el run loop reconcilia…
        app.swap_panes();
        let panes = norte_tui::panel::PaneSlots::new(
            Pane::new(vp("mem:///d"), Vec::new()),
            Pane::new(vp("mem:///d"), Vec::new()),
        );
        apply_cd(&panes, &mut f, &mut df, &mut lp, &mut sr, Cd::Swapped);
        // …y el MISMO call site cosecha a continuación.
        reap_search_run(&app, &mut sr);

        let s = sr.as_ref().expect("la búsqueda en curso NO se cancela");
        assert_eq!(s.pane, 1, "sigue los hits a su nuevo lado");
        assert!(
            app.panes[s.pane].virtual_search,
            "y ese lado es el que está en modo búsqueda"
        );
    }

    /// El watcher NO necesita reconciliado propio, y esto es lo que hace
    /// cierta esa afirmación: el conjunto vigilado se deriva de `app.panes`
    /// en cada vuelta del run loop (`rewatch(&watch_targets(app))` es la
    /// primera sentencia del bucle), así que basta con que `watch_targets`
    /// no cachee nada. Si alguien introdujera una copia por lado, el
    /// intercambio dejaría cada pane vigilando el dir del otro.
    #[test]
    fn watch_targets_sigue_a_los_panes_tras_el_intercambio() {
        let mut app = App::new(
            Pane::new(vp("file:///izq"), Vec::new()),
            Pane::new(vp("file:///der"), Vec::new()),
        );
        let antes = watch_targets(&app);
        app.swap_panes();
        let despues = watch_targets(&app);
        assert_eq!(
            antes[0], despues[1],
            "el dir izquierdo pasa a vigilarse a la derecha"
        );
        assert_eq!(antes[1], despues[0]);
        assert_ne!(antes[0], antes[1], "los dos dirs eran distintos de partida");
    }
}

#[cfg(test)]
mod refresh_ritual_tests {
    use super::{App, Fill, FillMsg, Pane, Probed, SearchRun, after_panes_refresh};
    use norte_proto::VPath;

    fn fill() -> Fill {
        let (_tx, rx) = tokio::sync::mpsc::channel::<FillMsg>(1);
        Fill { rx }
    }

    fn app() -> App {
        let d = VPath::parse("file:///d").expect("wire de test");
        App::new(Pane::new(d.clone(), Vec::new()), Pane::new(d, Vec::new()))
    }

    /// #118 (regresión pedida en el issue): un Esc a medias del refresh
    /// re-listó el pane 0 pero ABANDONÓ el 1 — el ritual solo puede soltar
    /// el drenador del pane re-listado de verdad; el del otro sigue drenando
    /// un listado que sigue siendo el suyo (#78).
    #[test]
    fn esc_a_medias_conserva_el_fill_del_pane_no_refrescado() {
        let mut app = app();
        let mut f: norte_frontend::layout::BySlot<Fill> = norte_frontend::layout::BySlot::new();
        f.insert(norte_tui::panel::SLOT_RIGHT, fill());
        let mut lp = Probed::from([(1, VPath::parse("file:///d/x").unwrap())]);
        let mut sr: Option<SearchRun> = None;
        after_panes_refresh(&mut app, [true, false], &mut f, &mut lp, &mut sr);
        assert!(
            f.get(norte_tui::panel::SLOT_RIGHT).is_some(),
            "el fill del pane 1 (no re-listado) sobrevive al Esc a medias"
        );
        assert!(lp.is_empty(), "la dedup de la sonda #52 caduca igualmente");
    }

    /// MAJOR-2: congelar impide que un veredicto cambie porque el lector se
    /// MUEVA, y eso está bien. Lo que no puede impedir es que cambie porque el
    /// MUNDO cambie: el brazo del `tick` no lleva guarda de overlay (a
    /// diferencia del de `dir_watch`, gateado por `watch_refresh_allowed`), así
    /// que una copia o un borrado que terminan con la ayuda abierta re-listan
    /// los dos panes y la entrada que los hechos describían puede haberse ido.
    /// La fila decía «no aplica a esta selección» de una selección que ya no
    /// existía.
    #[test]
    fn un_refresh_bajo_la_ayuda_abierta_recongela_los_hechos() {
        use norte_help::ChordResolver as _;

        let d = VPath::parse("file:///d").expect("wire de test");
        let fichero = norte_proto::Entry {
            attrs: std::collections::BTreeMap::new(),
            path: d.join(norte_proto::Segment::new(b"leeme.txt".to_vec()).expect("segmento")),
            kind: norte_proto::EntryKind::File,
            size: Some(3),
            mtime_ms: None,
        };
        let mut app = App::new(
            Pane::new(d.clone(), vec![fichero]),
            Pane::new(d, Vec::new()),
        );
        super::open_contextual_help(&mut app, norte_help::Lang::En, &[], None);
        assert!(
            app.help_chords.availability("pane.view").is_available(),
            "con un fichero bajo el cursor, F3 se puede pulsar"
        );

        // La tarea termina, el refresh entra por debajo del overlay y se lleva
        // por delante la entrada de la que hablaban los hechos.
        app.panes[0].refresh_listing(Vec::new());
        let mut f: norte_frontend::layout::BySlot<Fill> = norte_frontend::layout::BySlot::new();
        let mut lp = Probed::new();
        let mut sr: Option<SearchRun> = None;
        after_panes_refresh(&mut app, [true, false], &mut f, &mut lp, &mut sr);

        assert_eq!(
            app.help_chords.availability("pane.view").reason(),
            Some(norte_help::Reason::WrongTarget),
            "el listado cambió: los hechos tienen que volver a congelarse"
        );
    }
}
