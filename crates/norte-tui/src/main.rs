//! Binario del TUI (fases 3–4 M1): loop de eventos async sobre el core
//! EMBEBIDO o contra el DAEMON (fase 3 M2, por `[daemon] mode` o
//! `--daemon`), con keymap engine (ADR 0006). Regla 7: solo cambia el
//! transporte.
//! `ratatui::init/restore` gestionan raw mode + pantalla alternativa con
//! hook de pánico incluido: la terminal del usuario JAMÁS queda rota.
#![forbid(unsafe_code)]

use std::collections::VecDeque;
use std::sync::Arc;

use anyhow::{Context, Result};
use crossterm::event::{Event, EventStream, KeyCode, KeyModifiers};
use futures::StreamExt;
use norte_core::backend::EntryStream;
use norte_core::backend::{Backend, ConnEvent};
use norte_core::{Engine, TransferOptions};
use norte_i18n::{t, ta};
use norte_proto::DeleteMode;
use norte_proto::{Entry, EntryKind, Error, VPath};
use norte_tui::app::{
    App, DialogOutcome, ExtensionManager, Help, KeymapsError, Modal, Pane, PickerAction,
    TransferKind, config_error_category, detail_for_bar, dialog_key, error_category, error_message,
    io_error_category, keymaps_error_category, sort_entries, theme_error_category,
};
use norte_tui::config::{self, Layers, WatchMode};
use norte_tui::keymap::{COMMANDS, Chord, Effective, Resolution, Resolver, Screen, presets};
use norte_tui::lua::{
    CommandRun, Layer, LuaHost, PaneCtx, RunOutcome, StatusInput, TrustDecision, TrustStore,
};
use norte_tui::tasks::RetrySpec;
use norte_tui::ui;
use norte_tui::viewer::Viewer;
use norte_vfs_local::LocalProvider;
use sha2::Digest as _;
use tokio_util::sync::CancellationToken;

/// Filas que salta `cursor.page-up/down` (fijo hasta que el alto real del
/// pane viaje con el comando).
const PAGE: usize = 10;

/// Entradas de la PRIMERA página que un cd pinta antes de rellenar en
/// background (ADR 0017): con esto el primer render no espera al listado
/// entero (spec §11: primeras 100 en <16 ms aunque el dir tenga 500k).
const FIRST_PAGE: usize = 100;
/// Lote que el drenador coalesce antes de enviar (evita un re-sort por
/// entrada; el re-sort completo lo hace [`Pane::extend_listing`]). Un dir de
/// 100k son ~24 lotes ⇒ ~24 re-sorts de tamaño creciente durante el fill; el
/// merge incremental (claves persistidas) es la optimización diferida a issue.
const FILL_BATCH: usize = 4096;
/// El drenador vacía un lote PARCIAL cada tanto (además de al llenarlo): en un
/// listado remoto lento (páginas por RTT) el usuario ve progreso y el
/// contador `cargando… (n)` avanza en vez de saltar de 4096 en 4096.
const FILL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);

/// Mensaje del drenador de un listado paginado al run loop.
enum FillMsg {
    /// Un lote más de entradas para el pane.
    Batch(Vec<Entry>),
    /// El listado se cortó a mitad (error del provider/daemon): no es
    /// silencioso (la UI avisa y limpia el `loading`).
    Failed,
}

/// Un listado RELLENÁNDOSE en background: el pane destino y el canal del
/// drenador. Soltarlo (un cd nuevo) dropea el `rx` → el drenador muere en su
/// próximo envío → suelta el stream → cancelación cooperativa (regla 3).
struct Fill {
    pane: usize,
    rx: tokio::sync::mpsc::Receiver<FillMsg>,
}

/// Desenlace de un `cd`, para que el run loop actualice el relleno vivo.
enum Cd {
    /// El pane se reemplazó y su RESTO se rellena en background.
    Filling(Fill),
    /// El pane `usize` se reemplazó y ya está completo (o el cd falló): un
    /// relleno anterior de ESE pane queda obsoleto y hay que soltarlo.
    Replaced(usize),
    /// El cd se abandonó (Esc/Ctrl-C): nada cambió, el relleno sigue.
    Cancelled,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Args: preset posicional (`norte-tui vim`, capa MÁS alta sobre
    // norte.toml) + flags `--daemon`/`--socket` (fase 3 M2). Parseo a mano:
    // el TUI evita clap para no arrastrar su peso en el arranque.
    let (cli_preset, cli_daemon, cli_socket) = parse_args();
    let layers = config::standard_layers();
    let cfg = config::load_async(layers.clone())
        .await
        .context("config inválida")?;
    // Idioma: NORTE_LANG explícito > [ui] lang de la config > entorno.
    let lang = if std::env::var("NORTE_LANG").is_ok_and(|v| !v.is_empty()) {
        norte_i18n::Lang::from_env()
    } else if let Some(l) = &cfg.ui_lang {
        norte_i18n::Lang::negotiate(Some(l))
    } else {
        norte_i18n::Lang::from_env()
    };
    let _ = norte_i18n::force(lang);
    let (browse_eff, viewer_eff) = build_keymaps(&cfg, cli_preset.as_deref())?;

    let mut backend = make_backend(&cfg, cli_daemon, cli_socket).await?;

    let cwd = std::env::current_dir().context("cwd")?;
    // Deuda conocida: en Windows un cwd UNC (\\server\share) no es
    // representable todavía y esto aborta con error claro (issue #22).
    let start = norte_vfs_local::vpath_from_native(&cwd)
        .map_err(|e| anyhow::anyhow!("cwd no representable como VPath: {e}"))?;
    let left = Pane::new(
        start.clone(),
        backend
            .list(&start)
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?,
    );
    let right = Pane::new(
        start.clone(),
        backend
            .list(&start)
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?,
    );
    let mut app = App::new(left, right);
    apply_theme(&mut app, &cfg);
    // Canales del modo daemon (None en embebido): tasks de otros frontends
    // y avisos de (re)conexión — se drenan en el loop principal.
    let foreign_tasks = backend.take_foreign_tasks();
    let conn_events = backend.take_conn_events();
    let approvals = backend.take_approvals();
    let mut help_lines = norte_tui::help::build(&browse_eff, &viewer_eff);
    let mut resolver = Resolver::new(browse_eff);
    let mut viewer_resolver = Resolver::new(viewer_eff);

    // Hot-reload: vigilancia de las capas, con aviso si degrada a polling.
    let (cfg_tx, cfg_rx) = tokio::sync::mpsc::channel(8);
    let watch = config::watch(&layers, cfg_tx).await;
    if watch.mode == WatchMode::Polling {
        app.message = Some(t("msg-config-polling"));
    }

    let mut terminal = ratatui::init();
    let res = run(
        &mut terminal,
        &mut app,
        &backend,
        &mut resolver,
        &mut viewer_resolver,
        &mut help_lines,
        layers,
        cli_preset,
        cfg_rx,
        foreign_tasks,
        conn_events,
        approvals,
    )
    .await;
    ratatui::restore();
    drop(watch);
    res
}

/// Parsea los argumentos: preset posicional + `--daemon`/`--socket`.
fn parse_args() -> (Option<String>, bool, Option<std::path::PathBuf>) {
    let mut preset = None;
    let mut daemon = false;
    let mut socket = None;
    let mut it = std::env::args_os().skip(1);
    while let Some(arg) = it.next() {
        let a = arg.to_string_lossy();
        match a.as_ref() {
            "--daemon" => daemon = true,
            "--socket" => {
                socket = it.next().map(std::path::PathBuf::from);
            }
            s if s.starts_with("--") => {} // flag desconocido: ignora (compat)
            _ if preset.is_none() => preset = Some(a.into_owned()),
            _ => {}
        }
    }
    (preset, daemon, socket)
}

/// Elige el transporte (regla 7): `--daemon` o `[daemon] mode = daemon`
/// conecta al socket (arrancando `norte daemon run` si hace falta);
/// cualquier otra cosa = embebido (arranque instantáneo, el default).
async fn make_backend(
    cfg: &config::LoadedConfig,
    cli_daemon: bool,
    cli_socket: Option<std::path::PathBuf>,
) -> Result<Backend> {
    let want_daemon = cli_daemon || cfg.daemon_mode == Some(config::DaemonMode::Daemon);
    if !want_daemon {
        let engine = Engine::new();
        engine.register_provider(Arc::new(LocalProvider::os_root()));
        // Conexiones remotas (fase 6e): un path sftp://…/ftp://… navegable si
        // la host key ya es de confianza. La CONFIRMACIÓN TOFU interactiva
        // (modal con fingerprint) es UX pendiente — hoy un primer contacto
        // aparece como error con la huella; confírmalo con `norte connect`.
        engine.set_connector(Arc::new(norte_core::connect::ConnectionManager::new(
            norte_core::connect::config_dir(),
        )));
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
        let socket = match cli_socket.or_else(|| cfg.daemon_socket.clone()) {
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
                name: "norte-tui".into(),
                version: env!("CARGO_PKG_VERSION").into(),
            },
        )
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))
        .context("no se pudo hablar con el daemon")?;
        Ok(Backend::Remote(remote))
    }
}

/// Resuelve el preset (flag > config > default) y pliega las capas de
/// keymap (ADR 0007) para las DOS pantallas (browse y viewer). El error
/// tipado ([`KeymapsError`], #73) vive en `norte_tui::app` junto a su
/// categoría Fluent.
fn build_keymaps(
    cfg: &config::LoadedConfig,
    cli_preset: Option<&str>,
) -> Result<(Effective, Effective), KeymapsError> {
    let preset_name = cli_preset.unwrap_or(&cfg.preset);
    let presets = presets();
    let (_, preset) = presets
        .iter()
        .find(|(n, _)| *n == preset_name)
        .ok_or_else(|| KeymapsError::UnknownPreset {
            name: preset_name.to_owned(),
            available: presets
                .iter()
                .map(|(n, _)| *n)
                .collect::<Vec<_>>()
                .join(", "),
        })?;
    let invalid = |e: norte_tui::keymap::KeymapError| KeymapsError::Invalid {
        detail: e.to_string(),
    };
    let browse = Effective::build_for(preset, &cfg.keymap_layers, COMMANDS, Screen::Browse)
        .map_err(invalid)?;
    let viewer = Effective::build_for(preset, &cfg.keymap_layers, COMMANDS, Screen::Viewer)
        .map_err(invalid)?;
    Ok((browse, viewer))
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)] // wiring del binario, no API
async fn run(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    backend: &Backend,
    resolver: &mut Resolver,
    viewer_resolver: &mut Resolver,
    help_lines: &mut Vec<String>,
    layers: Layers,
    cli_preset: Option<String>,
    mut cfg_rx: tokio::sync::mpsc::Receiver<()>,
    mut foreign_tasks: Option<tokio::sync::mpsc::UnboundedReceiver<norte_core::backend::TaskRef>>,
    mut conn_events: Option<tokio::sync::mpsc::UnboundedReceiver<ConnEvent>>,
    mut approvals: Option<
        tokio::sync::mpsc::UnboundedReceiver<norte_proto::methods::PolicyApprovalRequired>,
    >,
) -> Result<()> {
    let mut events = EventStream::new();
    // Tick del panel de tasks: copia snapshots del watch (jamás bloquea).
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(100));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // Debounce del hot-reload SIN bloquear el loop (revisión fase 6): cada
    // evento de config empuja el deadline; el reload corre cuando vence.
    let mut reload_at: Option<tokio::time::Instant> = None;
    // Listado paginado rellenándose en background (ADR 0017): a lo sumo uno.
    let mut fill: Option<Fill> = None;
    // Scripting Lua (M4, ADR 0026): host por capas con trust TOFU. Como
    // `fill`, el estado vive en el run loop. El run en vuelo (a lo sumo UNO:
    // el estado Lua es uno) se pollea inline en el select — `CommandRun` es
    // !Send y este future corre en block_on, jamás en spawn.
    let mut lua_host = load_lua(app, &layers).await;
    let mut lua_run: Option<(CommandRun, CancellationToken)> = None;
    let mut lua_queue: VecDeque<String> = VecDeque::new();
    loop {
        // Barra Lua en cada vuelta, ANTES del draw (cacheada en el host).
        refresh_lua_status(app, lua_host.as_ref());
        // Exención puntual de la regla 2: el draw escribe stdout síncrono
        // (patrón async oficial de ratatui; acotado, runtime multi-thread).
        terminal.draw(|f| ui::draw(f, app))?;
        if app.quit {
            return Ok(());
        }
        tokio::select! {
            _ = tick.tick() => {
                // Un refresh de panes (mutación terminada) reescribe AMBOS
                // panes con el listado COMPLETO → suelta el relleno paginado
                // en curso (su drenador duplicaría entradas, BLOCKER del
                // rust-reviewer).
                if on_tick(app, backend, &mut events).await {
                    fill = None;
                }
            }
            Some(task) = async {
                match &mut foreign_tasks {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                // Task de OTRO frontend de la misma sesión (fase 3): al panel.
                app.board.push_foreign(task);
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
            msg = async {
                match &mut fill {
                    Some(f) => f.rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                // Lote del drenador del listado paginado (ADR 0017): al pane
                // que lo abrió. `None` = canal cerrado (fin del drenado).
                let pane = fill.as_ref().map_or(0, |f| f.pane);
                match msg {
                    Some(FillMsg::Batch(batch)) => app.panes[pane].extend_listing(batch),
                    Some(FillMsg::Failed) => {
                        app.panes[pane].finish_listing();
                        app.message = Some(t("msg-list-incomplete"));
                        fill = None;
                    }
                    None => {
                        app.panes[pane].finish_listing();
                        fill = None;
                    }
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
                reload_config(
                    app,
                    resolver,
                    viewer_resolver,
                    help_lines,
                    &layers,
                    cli_preset.as_deref(),
                )
                .await;
                // Hot-reload del scripting Lua (ADR 0026): host NUEVO entero
                // (jamás estado a medias). Un `CommandRun` en vuelo retiene
                // el estado VIEJO vía sus handles clonados (documentado en
                // `lua::api`) y no se toca; statusbar/estado renacen.
                lua_host = load_lua(app, &layers).await;
            }
            maybe = events.next() => {
                let Some(event) = maybe else { return Ok(()); };
                if let Event::Key(key) = event.context("evento de terminal")?
                    && key.kind == crossterm::event::KeyEventKind::Press
                {
                    app.message = None;
                    if app.theme_picker.is_some() {
                        on_theme_picker_key(app, key.modifiers, key.code).await;
                    } else if app.extensions.is_some() {
                        on_extensions_key(app, backend, key.modifiers, key.code).await;
                    } else if let Some(help) = &mut app.help {
                        // Teclas de la ayuda: fijas, como los diálogos (#24).
                        // ctrl+c conserva su significado global (salir).
                        match (key.modifiers, key.code) {
                            (KeyModifiers::CONTROL, KeyCode::Char('c')) => app.quit = true,
                            (
                                KeyModifiers::NONE,
                                KeyCode::Esc | KeyCode::Char('q') | KeyCode::F(1),
                            ) => app.help = None,
                            (KeyModifiers::NONE, KeyCode::Up) => help.scroll_up(1),
                            (KeyModifiers::NONE, KeyCode::Down) => help.scroll_down(1),
                            (KeyModifiers::NONE, KeyCode::PageUp) => help.scroll_up(PAGE),
                            (KeyModifiers::NONE, KeyCode::PageDown) => help.scroll_down(PAGE),
                            _ => {}
                        }
                    } else if app.modal.is_some() {
                        // El TOFU de Lua se resuelve AQUÍ (necesita el host,
                        // que vive en este loop): no navega ni toca `fill`.
                        if matches!(app.modal, Some(Modal::TrustLuaInit { .. })) {
                            resolve_lua_trust(app, lua_host.as_ref(), key.code).await;
                            continue;
                        }
                        // El modal TOFU (#45) puede NAVEGAR al confiar: su Cd
                        // se aplica igual que el de un comando.
                        match on_dialog_key(app, backend, &mut events, key.code).await {
                            Cd::Filling(f) => fill = Some(f),
                            Cd::Replaced(pane) => {
                                if fill.as_ref().is_some_and(|f| f.pane == pane) {
                                    fill = None;
                                }
                            }
                            Cd::Cancelled => {}
                        }
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
                            continue;
                        }
                        // Pantalla activa: el viewer tiene su contexto.
                        let active = if app.viewer.is_some() {
                            &mut *viewer_resolver
                        } else {
                            &mut *resolver
                        };
                        match active.push(Chord::from_event(key.modifiers, key.code)) {
                            Resolution::Run(cmd) => {
                                app.pending.clear();
                                // `lua:<nombre>` (M4): al despachador Lua —
                                // jamás a `dispatch` (no es comando fijo).
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
                                match dispatch(app, backend, &mut events, help_lines, &cmd).await {
                                    // Nuevo relleno: suelta el anterior (su rx
                                    // dropeado mata su drenador → suelta el
                                    // stream, regla 3).
                                    Cd::Filling(f) => fill = Some(f),
                                    // El pane se reemplazó sin relleno: invalida
                                    // uno anterior de ESE pane (no aplicaría).
                                    Cd::Replaced(pane) => {
                                        if fill.as_ref().is_some_and(|f| f.pane == pane) {
                                            fill = None;
                                        }
                                    }
                                    Cd::Cancelled => {}
                                }
                            }
                            Resolution::Pending(_) => {
                                app.pending = active
                                    .pending()
                                    .iter()
                                    .map(ToString::to_string)
                                    .collect::<Vec<_>>()
                                    .join(" ");
                            }
                            Resolution::Reset => app.pending.clear(),
                        }
                    }
                }
            }
        }
        // Resize/Focus/etc: el draw del inicio del loop repinta solo.
    }
}

/// Hot-reload (ADR 0007): relee TODAS las capas; ante CUALQUIER error se
/// conserva la config vigente y se avisa por la barra — jamás romper una
/// sesión en marcha por un TOML a medio guardar.
/// Resuelve `[ui].theme` (preset o ruta) y lo aplica al `App`; ante error
/// degrada al preset por defecto y avisa (ADR 0020). El frontend no revienta
/// por un tema malo.
fn apply_theme(app: &mut App, cfg: &config::LoadedConfig) {
    let depth = norte_tui::theme::detect_depth();
    match norte_tui::theme::resolve(cfg.ui_theme.as_deref(), depth) {
        Ok(theme) => app.theme = theme,
        Err(e) => {
            app.theme = norte_tui::theme::TuiTheme::default();
            // Por categoría Fluent (#73): jamás el Display del OS ni el
            // diagnóstico crudo (el spec puede venir de un `./.norte` ajeno).
            app.message = Some(theme_error_category(&e));
        }
    }
}

/// Traduce las teclas del popup de tema a una acción de dominio (la lógica
/// vive en `App`, testeable). Fijas como los demás overlays (#24); `ctrl+c`
/// conserva su salida global. Al confirmar, PERSISTE la elección en el
/// `norte.toml` del usuario (ADR 0020), sin bloquear el runtime.
async fn on_theme_picker_key(app: &mut App, mods: KeyModifiers, code: KeyCode) {
    let action = match (mods, code) {
        (KeyModifiers::CONTROL, KeyCode::Char('c')) => {
            app.quit = true;
            return;
        }
        (KeyModifiers::NONE, KeyCode::Up | KeyCode::Char('k')) => PickerAction::Up,
        (KeyModifiers::NONE, KeyCode::Down | KeyCode::Char('j')) => PickerAction::Down,
        (KeyModifiers::NONE, KeyCode::Enter) => PickerAction::Confirm,
        (KeyModifiers::NONE, KeyCode::Esc | KeyCode::F(9)) => PickerAction::Cancel,
        _ => return,
    };
    // El nombre a persistir se toma ANTES de que Confirm cierre el popup.
    let confirmed = (action == PickerAction::Confirm)
        .then(|| {
            app.theme_picker
                .as_ref()
                .and_then(|p| p.selected().map(String::from))
        })
        .flatten();
    app.theme_picker_input(action);
    if let Some(name) = confirmed {
        // I/O en spawn_blocking: el runtime jamás se bloquea (regla 2).
        let n = name.clone();
        match tokio::task::spawn_blocking(move || config::persist_ui_theme(&n)).await {
            Ok(Ok(path)) => {
                // El path deriva de XDG_CONFIG_HOME/APPDATA (entorno):
                // saneado como cualquier detalle (#73).
                app.message = Some(ta(
                    "msg-theme-saved",
                    &[
                        ("name", &name),
                        ("path", &detail_for_bar(&path.display().to_string())),
                    ],
                ));
            }
            Ok(Err(e)) => {
                // El tema YA se aplicó (sesión); solo no se pudo guardar. A
                // la barra va la CATEGORÍA, jamás el Display del OS (#73).
                app.message = Some(ta(
                    "msg-theme-save-failed",
                    &[("error", &io_error_category(&e))],
                ));
            }
            // Un panic en el write es un bug nuestro: que no tumbe la TUI.
            Err(_) => {}
        }
    }
}

/// Teclas del overlay de extensiones (M4-P3), fijas como los demás overlays
/// (#24); `ctrl+c` conserva su salida global. Regla 7: aprobar/activar viaja
/// al core por el `Backend`; el bool LOCAL solo se togglea tras un OK (feedback
/// inmediato sin relistar). El id y el estado se toman ANTES del `.await` (el
/// borrow del `mgr` se suelta durante la llamada al backend y se re-obtiene
/// después para reflejar el resultado).
async fn on_extensions_key(app: &mut App, backend: &Backend, mods: KeyModifiers, code: KeyCode) {
    if mods.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
        app.quit = true;
        return;
    }
    let Some(mgr) = &mut app.extensions else {
        return;
    };
    match code {
        KeyCode::Up | KeyCode::Char('k') => mgr.up(),
        KeyCode::Down | KeyCode::Char('j') => mgr.down(),
        KeyCode::Esc | KeyCode::Char('q') => app.extensions = None,
        KeyCode::Char('a') => {
            // Id y estado ANTES del await (suelta el borrow de `mgr`).
            let Some((id, cur)) = mgr.selected().map(|p| (p.id.clone(), p.approved)) else {
                return;
            };
            match backend.plugins_set_approval(&id, !cur).await {
                Ok(()) => {
                    if let Some(mgr) = &mut app.extensions {
                        mgr.set_local_approved(!cur);
                    }
                }
                Err(e) => app.message = Some(error_message(&e)),
            }
        }
        KeyCode::Char('e') => {
            let Some((id, cur)) = mgr.selected().map(|p| (p.id.clone(), p.enabled)) else {
                return;
            };
            match backend.plugins_set_enabled(&id, !cur).await {
                Ok(()) => {
                    if let Some(mgr) = &mut app.extensions {
                        mgr.set_local_enabled(!cur);
                    }
                }
                Err(e) => app.message = Some(error_message(&e)),
            }
        }
        _ => {}
    }
}

async fn reload_config(
    app: &mut App,
    resolver: &mut Resolver,
    viewer_resolver: &mut Resolver,
    help_lines: &mut Vec<String>,
    layers: &Layers,
    cli_preset: Option<&str>,
) {
    match config::load_async(layers.clone()).await {
        Ok(cfg) => match build_keymaps(&cfg, cli_preset) {
            Ok((browse, viewer)) => {
                // La ayuda refleja el keymap VIGENTE: se reconstruye aquí.
                *help_lines = norte_tui::help::build(&browse, &viewer);
                app.help = None;
                *resolver = Resolver::new(browse);
                *viewer_resolver = Resolver::new(viewer);
                app.pending.clear();
                app.message = Some(t("msg-config-reloaded"));
                // El tema también es hot-reloadable (ADR 0020): si falla, el
                // mensaje de error del tema pisa el de "config recargada".
                apply_theme(app, &cfg);
            }
            Err(e) => {
                app.message = Some(ta(
                    "msg-config-not-applied",
                    &[("error", &keymaps_error_category(&e))],
                ));
            }
        },
        Err(e) => {
            app.message = Some(ta(
                "msg-config-not-applied",
                &[("error", &config_error_category(&e))],
            ));
        }
    }
}

/// Tope de la cola FIFO de comandos Lua (M4): con un run en vuelo, los
/// siguientes se encolan hasta aquí; llena, solo queda el aviso.
const LUA_QUEUE_MAX: usize = 8;

/// Directorio de ESTADO del usuario: `$XDG_STATE_HOME/norte` o
/// `~/.local/state/norte` (Windows: `%LOCALAPPDATA%\norte\state`). Sin
/// precedente en el workspace (verificado 2026-07-18: ningún uso de
/// `XDG_STATE_HOME`; la config usa `XDG_CONFIG_HOME` —
/// `config::user_config_dir`): el trust store de Lua es ESTADO local de la
/// máquina, no config que deba viajar con los dotfiles. `None` si el
/// entorno no define nada (CI pelada): el caller degrada con aviso
/// (fail-closed para el TOFU — sin store no corre el script de proyecto).
fn state_dir() -> Option<std::path::PathBuf> {
    use std::path::PathBuf;
    if cfg!(windows) {
        return std::env::var_os("LOCALAPPDATA")
            .map(|d| PathBuf::from(d).join("norte").join("state"));
    }
    if let Some(xdg) = std::env::var_os("XDG_STATE_HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(xdg).join("norte"));
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state/norte"))
}

/// Etiqueta ESTABLE de una capa para `err-lua-load` (no localizada: es un
/// identificador de capa, no prosa).
fn lua_layer_label(layer: Layer) -> &'static str {
    match layer {
        Layer::System => "system",
        Layer::User => "user",
        Layer::Project => "project",
    }
}

/// Evalúa una capa en el host y enruta error/warnings a la barra
/// (`err-lua-load`; con varios, el último gana el hueco — ok v1). El
/// detalle es diagnóstico CRUDO del runtime Lua: SIEMPRE por
/// `detail_for_bar` (patrón #73).
fn eval_lua_layer(app: &mut App, host: &LuaHost, source: &[u8], layer: Layer) {
    let label = lua_layer_label(layer);
    match host.eval_layer(source, layer) {
        Ok(warnings) => {
            for w in warnings {
                app.message = Some(ta(
                    "err-lua-load",
                    &[("layer", label), ("detail", &detail_for_bar(&w.detail))],
                ));
            }
        }
        Err(e) => {
            app.message = Some(ta(
                "err-lua-load",
                &[
                    ("layer", label),
                    ("detail", &detail_for_bar(&e.to_string())),
                ],
            ));
        }
    }
}

/// Lee `path` si existe (`spawn_blocking`, regla 2): `None` = capa ausente.
async fn read_optional_bytes(path: std::path::PathBuf) -> std::io::Result<Option<Vec<u8>>> {
    match tokio::task::spawn_blocking(move || std::fs::read(&path)).await {
        Ok(Ok(bytes)) => Ok(Some(bytes)),
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Ok(Err(e)) => Err(e),
        // Un panic leyendo es un bug NUESTRO: que reviente visible (mismo
        // criterio que `config::load_async`).
        Err(e) => std::panic::resume_unwind(e.into_panic()),
    }
}

/// Carga los `init.lua` por capas (ADR 0007 + 0026): sistema y usuario se
/// evalúan directo (config PROPIA del usuario); el de PROYECTO (`./.norte`,
/// la ÚLTIMA capa, como en `config::standard_layers`) pasa por el trust
/// TOFU ([`load_lua_project`]). La carga NO toca el `Backend` (solo evalúa
/// código; el FS de los comandos llega en `invoke`). Errores/warnings van a
/// la barra por categoría; una capa rota no impide las demás.
///
/// Devuelve `None` si mlua no pudo ni arrancar: el scripting queda
/// deshabilitado con aviso — el TUI sigue.
///
/// En hot-reload se llama de nuevo y el host RENACE entero (un `CommandRun`
/// en vuelo retiene el estado viejo vía sus handles — documentado en
/// `lua::api`); un `TrustLuaInit` pendiente de la carga anterior queda
/// obsoleto y se cierra (sus bytes ya no son lo que se evaluaría).
async fn load_lua(app: &mut App, layers: &Layers) -> Option<LuaHost> {
    if matches!(app.modal, Some(Modal::TrustLuaInit { .. })) {
        app.modal = None;
        app.open_next_pending();
    }
    app.lua_pending_trust = None;

    let host = match LuaHost::new() {
        Ok(h) => h,
        Err(e) => {
            app.message = Some(ta(
                "err-lua-load",
                &[
                    ("layer", "host"),
                    ("detail", &detail_for_bar(&e.to_string())),
                ],
            ));
            return None;
        }
    };
    let last = layers.dirs.len().saturating_sub(1);
    for (i, dir) in layers.dirs.iter().enumerate() {
        // Mismo orden que la config: la última capa es la de proyecto.
        let layer = if i == last {
            Layer::Project
        } else if i == 0 {
            Layer::System
        } else {
            Layer::User
        };
        if layer == Layer::Project {
            load_lua_project(app, &host, dir.clone()).await;
        } else {
            match read_optional_bytes(dir.join("init.lua")).await {
                Ok(Some(bytes)) => eval_lua_layer(app, &host, &bytes, layer),
                Ok(None) => {}
                Err(e) => {
                    app.message = Some(ta(
                        "err-lua-load",
                        &[
                            ("layer", lua_layer_label(layer)),
                            ("detail", &io_error_category(&e)),
                        ],
                    ));
                }
            }
        }
    }
    Some(host)
}

/// Resultado de la lectura VERIFICADA del `init.lua` de proyecto.
enum ProjectLua {
    /// No hay `./.norte/init.lua` (o `.norte` no es un directorio): nada.
    Absent,
    /// `.norte` o `init.lua` son SYMLINKS (criterio de seguridad de la
    /// review de T6): un symlink a un proyecto ya trusted ejecutaría
    /// contenido aprobado para OTRO sitio en un contexto hostil. No se
    /// carga, con aviso.
    Symlink,
    /// io real (permisos, etc.).
    Io(std::io::Error),
    /// Path CANÓNICO + bytes leídos UNA sola vez.
    Ready(std::path::PathBuf, Vec<u8>),
}

/// Chequeos + lectura del script de proyecto, todo síncrono en un bloque
/// (se llama bajo `spawn_blocking`): `symlink_metadata` verifica que `.norte`
/// es directorio REAL y que `init.lua` es fichero REGULAR — jamás a través
/// de un symlink. La ventana entre check y `read` no es cero (no hay
/// `O_NOFOLLOW` portable aquí), pero el contenido LEÍDO es exactamente lo que
/// se aprueba y evalúa (anti-TOCTOU del contenido; el residual es del path).
/// El path se CANONICALIZA para que check y record usen siempre la misma
/// forma (limitación NFC/NFD del store, documentada en `lua::trust`).
fn project_lua_read(dir: &std::path::Path) -> ProjectLua {
    let dir_md = match std::fs::symlink_metadata(dir) {
        Ok(md) => md,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return ProjectLua::Absent,
        Err(e) => return ProjectLua::Io(e),
    };
    if dir_md.file_type().is_symlink() {
        return ProjectLua::Symlink;
    }
    if !dir_md.is_dir() {
        return ProjectLua::Absent;
    }
    let file = dir.join("init.lua");
    let file_md = match std::fs::symlink_metadata(&file) {
        Ok(md) => md,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return ProjectLua::Absent,
        Err(e) => return ProjectLua::Io(e),
    };
    if file_md.file_type().is_symlink() {
        return ProjectLua::Symlink;
    }
    if !file_md.is_file() {
        return ProjectLua::Absent;
    }
    let canon = match std::fs::canonicalize(&file) {
        Ok(c) => c,
        Err(e) => return ProjectLua::Io(e),
    };
    match std::fs::read(&file) {
        Ok(bytes) => ProjectLua::Ready(canon, bytes),
        Err(e) => ProjectLua::Io(e),
    }
}

/// Capa de PROYECTO (ADR 0026): verifica symlinks, consulta el
/// [`TrustStore`] (`state_dir()/lua-trust.toml`, `spawn_blocking`) y decide
/// — `Trusted` evalúa; `Denied` CALLA; `DeniedPathChanged` avisa (deny
/// silencioso, JAMÁS modal automático: reabrirlo en cada edición de un
/// script ya rechazado acabaría en aprobación por fatiga); Unknown abre el
/// modal TOFU dejando los bytes pendientes en `App::lua_pending_trust`.
async fn load_lua_project(app: &mut App, host: &LuaHost, dir: std::path::PathBuf) {
    let read = match tokio::task::spawn_blocking(move || project_lua_read(&dir)).await {
        Ok(r) => r,
        Err(e) => std::panic::resume_unwind(e.into_panic()),
    };
    let (path, bytes) = match read {
        ProjectLua::Absent => return,
        ProjectLua::Symlink => {
            app.message = Some(t("msg-lua-symlink"));
            return;
        }
        ProjectLua::Io(e) => {
            app.message = Some(ta(
                "err-lua-load",
                &[("layer", "project"), ("detail", &io_error_category(&e))],
            ));
            return;
        }
        ProjectLua::Ready(path, bytes) => (path, bytes),
    };
    let Some(state) = state_dir() else {
        // Sin dir de estado no hay store; sin store no hay TOFU; sin TOFU el
        // script de proyecto NO corre (fail-closed) — con aviso.
        app.message = Some(ta(
            "err-lua-load",
            &[
                ("layer", "project"),
                ("detail", "no state dir (XDG_STATE_HOME/HOME)"),
            ],
        ));
        return;
    };
    let store_path = state.join("lua-trust.toml");
    let (check_path, check_bytes) = (path.clone(), bytes.clone());
    let decision = match tokio::task::spawn_blocking(move || {
        TrustStore::open(store_path).map(|s| s.check(&check_path, &check_bytes))
    })
    .await
    {
        Ok(Ok(d)) => d,
        Ok(Err(e)) => {
            // Store ilegible/corrupto: fail-closed (podría ser el rastro de
            // una manipulación, no una ausencia benigna — `lua::trust`).
            app.message = Some(ta(
                "err-lua-load",
                &[
                    ("layer", "project"),
                    ("detail", &detail_for_bar(&e.to_string())),
                ],
            ));
            return;
        }
        Err(e) => std::panic::resume_unwind(e.into_panic()),
    };
    match decision {
        TrustDecision::Trusted => eval_lua_layer(app, host, &bytes, Layer::Project),
        TrustDecision::Denied => {}
        TrustDecision::DeniedPathChanged => app.message = Some(t("msg-lua-denied-changed")),
        TrustDecision::Unknown => {
            if app.modal.is_some() {
                // Otro modal abierto (solo alcanzable en hot-reload): ni se
                // pisa ni se encola (v1) — el próximo reload re-pregunta.
                return;
            }
            let hash = sha2::Sha256::digest(&bytes);
            let hash_abbrev = hash.iter().take(4).fold(String::new(), |mut s, b| {
                use std::fmt::Write as _;
                let _ = write!(s, "{b:02x}");
                s
            });
            app.modal = Some(Modal::TrustLuaInit {
                // Saneado AQUÍ (contrato del modal: `path` ya listo para
                // pintar) — un path de repo ajeno puede traer bidi/control.
                path: detail_for_bar(&path.display().to_string()),
                hash_abbrev,
            });
            app.lua_pending_trust = Some((path, bytes));
        }
    }
}

/// Resuelve el modal [`Modal::TrustLuaInit`] (interceptado en el run loop,
/// que es quien tiene el host): `y` confía, `n`/Esc deniegan, Enter NO
/// decide (`dialog_key`). La decisión se PERSISTE en el [`TrustStore`]
/// (`spawn_blocking`, regla 2) y, si aprueba, se evalúan los BYTES guardados
/// en `App::lua_pending_trust` — lo aprobado = lo evaluado (anti-TOCTOU),
/// jamás una relectura de disco. Un fallo al persistir no bloquea la
/// decisión de ESTA sesión (solo re-preguntará la próxima): aviso y sigue.
async fn resolve_lua_trust(app: &mut App, host: Option<&LuaHost>, code: KeyCode) {
    let Some(modal) = app.modal.clone() else {
        return;
    };
    let allow = match dialog_key(&modal, code) {
        DialogOutcome::Confirmed => true,
        DialogOutcome::Cancelled => false,
        DialogOutcome::Open | DialogOutcome::Retry(_) => return,
    };
    app.modal = None;
    if let Some((path, bytes)) = app.lua_pending_trust.take() {
        let (rec_path, rec_bytes) = (path.clone(), bytes.clone());
        let record = tokio::task::spawn_blocking(move || -> std::io::Result<()> {
            let dir = state_dir().ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::NotFound, "sin directorio de estado")
            })?;
            let mut store = TrustStore::open(dir.join("lua-trust.toml"))?;
            store.record(&rec_path, &rec_bytes, allow)
        })
        .await;
        // `Ok(Ok)` = persistido; `Err(_)` (panic al persistir) es bug
        // nuestro y no tumba la TUI — la decisión de ESTA sesión ya vale.
        if let Ok(Err(e)) = record {
            app.message = Some(ta(
                "err-lua-load",
                &[("layer", "project"), ("detail", &io_error_category(&e))],
            ));
        }
        if allow && let Some(host) = host {
            eval_lua_layer(app, host, &bytes, Layer::Project);
        }
    }
    app.open_next_pending();
}

/// Arranca el comando Lua `name` con el snapshot ACTUAL de panes como
/// `PaneCtx` (congelado: determinismo > frescura). El TUI no tiene
/// multi-selección todavía: `selection` = la entrada bajo el cursor (o
/// vacía) — documentado, mismo dato que `current`. Comando no registrado →
/// barra `err-lua-unknown` (no es error de keymap) y `None`.
fn start_lua_run(
    app: &mut App,
    host: &LuaHost,
    backend: &Backend,
    name: &str,
) -> Option<(CommandRun, CancellationToken)> {
    let pane = app.focused();
    let other = &app.panes[1 - app.focus()];
    let current = pane.selected().map(|e| e.path.clone());
    let ctx = PaneCtx {
        cwd: pane.dir.clone(),
        other_cwd: other.dir.clone(),
        selection: current.clone().into_iter().collect(),
        current,
    };
    let token = CancellationToken::new();
    let Some(run) = host.invoke(name, backend.clone(), ctx, token.clone()) else {
        app.message = Some(ta("err-lua-unknown", &[("name", &detail_for_bar(name))]));
        return None;
    };
    Some((run, token))
}

/// Despacha un binding `lua:<nombre>`: con un run en vuelo lo ENCOLA (FIFO,
/// tope [`LUA_QUEUE_MAX`]; llena = solo el aviso); libre, arranca. Sin host
/// (mlua no arrancó) el comando no puede existir → `err-lua-unknown`.
fn run_lua_command(
    app: &mut App,
    lua_host: Option<&LuaHost>,
    backend: &Backend,
    name: &str,
    lua_run: &mut Option<(CommandRun, CancellationToken)>,
    lua_queue: &mut VecDeque<String>,
) {
    let Some(host) = lua_host else {
        app.message = Some(ta("err-lua-unknown", &[("name", &detail_for_bar(name))]));
        return;
    };
    if lua_run.is_some() {
        if lua_queue.len() < LUA_QUEUE_MAX {
            lua_queue.push_back(name.to_owned());
        }
        app.message = Some(t("msg-lua-busy"));
        return;
    }
    *lua_run = start_lua_run(app, host, backend, name);
}

/// Recalcula la barra Lua (hook `norte.ui.statusbar`) con el snapshot del
/// pane con foco. El host cachea por `PartialEq` y CONGELA con un run en
/// vuelo (ver `lua::api`); su salida ya viene saneada. Un fallo del hook
/// (take-once) sale una vez por la barra y el hook queda deshabilitado
/// hasta el próximo hot-reload.
fn refresh_lua_status(app: &mut App, lua_host: Option<&LuaHost>) {
    let Some(host) = lua_host else {
        app.lua_status = None;
        return;
    };
    let pane = app.focused();
    let input = StatusInput {
        cwd: pane.dir.to_wire().into_bytes(),
        selected: pane.cursor,
        // Sin multi-selección: los bytes de la entrada bajo el cursor.
        selected_bytes: pane.selected().and_then(|e| e.size).unwrap_or(0),
        entries: pane.entries.len(),
        tasks: app
            .board
            .rows()
            .iter()
            .filter(|r| !r.last.state.is_terminal())
            .count(),
    };
    app.lua_status = host.statusbar(&input);
    if let Some(detail) = host.statusbar_error() {
        app.message = Some(ta(
            "err-lua-statusbar",
            &[("detail", &detail_for_bar(&detail))],
        ));
    }
}

/// Tick: refresca snapshots del panel y reacciona a las tasks que ACABAN
/// de terminar — colisión con contexto → a la COLA de diálogos (jamás se
/// pisa un modal abierto, hallazgo B1); el resto → mensaje por categoría +
/// refresh de ambos panes (una mutación pudo cambiarlos).
/// (Strings de mensaje hardcodeados hasta Fluent — fase 9, issue #1.)
/// Devuelve `true` si ha REFRESCADO los panes (una mutación terminó): el run
/// loop suelta entonces cualquier relleno paginado en curso — `refresh_panes`
/// reescribe AMBOS panes con el listado completo, así que un drenador viejo
/// duplicaría entradas si siguiera vivo.
async fn on_tick(app: &mut App, backend: &Backend, events: &mut EventStream) -> bool {
    let finished = app.board.tick();
    if finished.is_empty() {
        app.open_next_pending();
        return false;
    }
    let mut refresh = false;
    for fin in finished {
        use norte_proto::TaskState;
        match fin.state {
            TaskState::Completed => {
                refresh = true;
                app.message = Some(t("msg-done"));
            }
            TaskState::Cancelled => {
                refresh = true;
                app.message = Some(t("msg-cancelled"));
            }
            TaskState::Failed { error } => {
                if let (Error::Unsupported, Some(target)) = (&error, &fin.trash_target) {
                    // La papelera no pudo AQUÍ (mount sin topdir…): se
                    // reofrece PERMANENTE con aviso — degradación con
                    // usuario informado (ADR 0009), jamás pisando un modal.
                    if app.modal.is_none() {
                        app.modal = Some(Modal::ConfirmDelete {
                            target: target.clone(),
                            permanent: true,
                        });
                    } else {
                        app.message = Some(t("msg-no-trash-here"));
                    }
                } else if let (Error::Conflict { .. }, Some(retry)) = (&error, fin.retry) {
                    app.pending_collisions.push_back(retry);
                } else {
                    // Render por CATEGORÍA localizado (spec §17.7, #20):
                    // jamás el Display inglés ni strings del OS.
                    app.message = Some(error_message(&error));
                    refresh = true;
                }
            }
            _ => {}
        }
    }
    app.open_next_pending();
    if refresh {
        refresh_panes(app, backend, events).await;
    }
    refresh
}

/// Recarga ambos panes tras una mutación (pueden mostrar el mismo dir).
/// CANCELABLE como el cd (regla 3): Esc abandona el refresh (los panes se
/// quedan como estaban), Ctrl-C sale. El cursor se conserva por ÍNDICE
/// (tras un delete queda en la siguiente entrada — semántica ortodoxa).
async fn refresh_panes(app: &mut App, backend: &Backend, events: &mut EventStream) {
    for i in 0..app.panes.len() {
        let dir = app.panes[i].dir.clone();
        let fut = listing(backend, &dir);
        tokio::pin!(fut);
        loop {
            tokio::select! {
                res = &mut fut => {
                    match res {
                        Ok(entries) => {
                            let pane = &mut app.panes[i];
                            let cursor = pane.cursor.min(entries.len().saturating_sub(1));
                            pane.entries = entries;
                            pane.cursor = cursor;
                            // El listado es COMPLETO: si venía de un cd paginado
                            // a medio rellenar, ya no está cargando (el run loop
                            // suelta el drenador tras este refresh).
                            pane.loading = false;
                        }
                        // Sin silencio: el dir pudo desaparecer (issue #20).
                        Err(e) => app.message = Some(ta("msg-refresh-error", &[("error", &error_category(&e))])),
                    }
                    break;
                }
                maybe = events.next() => {
                    match maybe {
                        Some(Ok(Event::Key(key)))
                            if key.kind == crossterm::event::KeyEventKind::Press =>
                        {
                            match (key.code, key.modifiers) {
                                (KeyCode::Char('c'), m) if m.contains(KeyModifiers::CONTROL) => {
                                    app.quit = true;
                                    return;
                                }
                                (KeyCode::Esc, _) => return,
                                _ => {}
                            }
                        }
                        Some(Ok(_)) => {}
                        Some(Err(_)) | None => return,
                    }
                }
            }
        }
    }
}

/// Teclas de un modal abierto (hardcodeadas, issue #24). `events` es para el
/// reintento de navegación del modal TOFU (#45): confiar en la host key
/// relanza el `cd`, que tiene su propio loop de eventos.
async fn on_dialog_key(
    app: &mut App,
    backend: &Backend,
    events: &mut EventStream,
    code: KeyCode,
) -> Cd {
    let Some(modal) = app.modal.clone() else {
        return Cd::Cancelled;
    };
    match dialog_key(&modal, code) {
        DialogOutcome::Open => {}
        DialogOutcome::Cancelled => {
            app.modal = None;
            app.open_next_pending();
            // Cerrar el diálogo de aprobación ES denegar (fail-safe): el
            // agente recibe `not-approved`, jamás una espera colgada.
            if let Modal::ApproveAgentOp { req } = modal {
                decide_approval(app, backend, req.approval_id, false).await;
            }
        }
        DialogOutcome::Confirmed => {
            app.modal = None;
            // OJO (MAJOR del rust-reviewer): NO abrir la siguiente pendiente
            // ANTES del match — el retry TOFU (`return cd`) puede reabrir un
            // TrustHostKey y PISAR una aprobación de agente ya sacada de la
            // cola (quedaría huérfana hasta su TTL). Se difiere al final.
            match modal {
                Modal::ConfirmDelete { target, permanent } => {
                    let mode = if permanent {
                        DeleteMode::Permanent
                    } else {
                        DeleteMode::Trash
                    };
                    match backend.delete(&target, mode).await {
                        Ok(task) => {
                            app.board
                                .push_full(task, None, (!permanent).then(|| target.clone()));
                        }
                        Err(e) => app.message = Some(error_message(&e)),
                    }
                }
                Modal::ConfirmTransfer { kind, from, to } => {
                    submit_transfer(app, backend, kind, from, to, TransferOptions::default()).await;
                }
                // TrustLuaInit se intercepta ANTES en el run loop (necesita
                // el LuaHost): inalcanzable aquí — no-op defensivo.
                Modal::Collision { .. } | Modal::TrustLuaInit { .. } => {}
                Modal::ApproveAgentOp { req } => {
                    decide_approval(app, backend, req.approval_id, true).await;
                }
                // TOFU (#45): confía en la host key y REINTENTA la navegación.
                Modal::TrustHostKey {
                    host,
                    port,
                    algo,
                    fingerprint,
                    dir,
                } => {
                    match backend
                        .trust_host_key(&host, port, &algo, &fingerprint)
                        .await
                    {
                        Ok(()) => {
                            // El engine re-verifica el fingerprint contra la
                            // clave que el host presenta AHORA (anti-TOCTOU,
                            // ADR 0015 D); si aún falla, el retry lo mostrará.
                            let outcome = cd(app, backend, events, dir).await;
                            // Solo abrir la siguiente pendiente si el retry NO
                            // dejó un modal (otro HostKeyUnknown): jamás pisar.
                            if app.modal.is_none() {
                                app.open_next_pending();
                            }
                            return outcome;
                        }
                        Err(e) => app.message = Some(error_message(&e)),
                    }
                }
            }
            // Todas las ramas salvo el retry TOFU (que ya volvió) abren aquí
            // la siguiente pendiente, con el modal ya cerrado.
            app.open_next_pending();
        }
        DialogOutcome::Retry(policy) => {
            app.modal = None;
            if let Modal::Collision { retry } = modal {
                // Conserva las opciones ORIGINALES; solo cambia la política.
                let opts = TransferOptions {
                    on_collision: policy,
                    ..retry.opts
                };
                submit_transfer(app, backend, retry.kind, retry.from, retry.to, opts).await;
            }
            app.open_next_pending();
        }
    }
    // Salvo el retry TOFU (que hace `return cd(...)`), un modal no navega.
    Cd::Cancelled
}

/// Resuelve una aprobación de policy (`policy.decide`, M3-3b T5). Un error
/// (id ya vencido/decidido por otro frontend, daemon caído) sale por la
/// barra: la pendiente, si sigue viva, vencerá por TTL — jamás se cuelga.
async fn decide_approval(app: &mut App, backend: &Backend, approval_id: u64, approve: bool) {
    if let Err(e) = backend.policy_decide(approval_id, approve).await {
        app.message = Some(error_message(&e));
    }
}

/// Encola una transferencia y la registra en el panel con su contexto de
/// reintento (para el diálogo de colisión).
async fn submit_transfer(
    app: &mut App,
    backend: &Backend,
    kind: TransferKind,
    from: VPath,
    to: VPath,
    opts: TransferOptions,
) {
    let res = match kind {
        TransferKind::Copy => backend.copy(&from, &to, opts).await,
        TransferKind::Move => backend.move_(&from, &to, opts).await,
    };
    match res {
        Ok(task) => app.board.push(
            task,
            Some(RetrySpec {
                kind,
                from,
                to,
                opts,
            }),
        ),
        Err(e) => app.message = Some(error_message(&e)),
    }
}

/// Ejecuta un comando nombrado (ADR 0006: los mismos nombres que verán la
/// palette y el wire). Un error de listado en un cd NO tumba el TUI: el
/// pane se queda donde estaba (aviso visible: barra de mensajes, issue #20).
#[allow(clippy::too_many_lines)] // tabla de despacho comando→efecto, no API
async fn dispatch(
    app: &mut App,
    backend: &Backend,
    events: &mut EventStream,
    help_lines: &[String],
    cmd: &str,
) -> Cd {
    // Solo los cd (nav.enter/nav.parent) tocan el relleno en background; el
    // resto de comandos lo dejan como está (`Cancelled`).
    let mut cd_outcome = Cd::Cancelled;
    match cmd {
        "app.quit" => app.quit = true,
        "pane.switch" => app.switch_focus(),
        "cursor.up" => app.focused_mut().move_up(1),
        "cursor.down" => app.focused_mut().move_down(1),
        "cursor.page-up" => app.focused_mut().move_up(PAGE),
        "cursor.page-down" => app.focused_mut().move_down(PAGE),
        "cursor.top" => app.focused_mut().move_to_start(),
        "cursor.bottom" => app.focused_mut().move_to_end(),
        "nav.enter" => {
            // También symlinks: si apunta a un dir, el provider listará; si
            // no, el cd falla y se absorbe — qué es "entrable" lo decide el
            // core, no el TUI (regla 7). Un File .zip/.tar entra como
            // directorio virtual (ADR 0018): el TUI solo COMPONE el path
            // (azúcar de navegación); listar/validar sigue siendo del core.
            let target = app
                .focused()
                .selected()
                .filter(|e| matches!(e.kind, EntryKind::Dir | EntryKind::Symlink))
                .map(|e| e.path.clone())
                .or_else(|| app.focused().selected().and_then(archive_root_for));
            if let Some(dir) = target {
                cd_outcome = cd(app, backend, events, dir).await;
            }
        }
        "nav.parent" => {
            // Salir de la raíz interior de un archivo = el dir que CONTIENE
            // al contenedor (el padre sintáctico sería un compuesto sin
            // marcador: malformado, ADR 0018).
            let dir = &app.focused().dir;
            let parent = match dir.archive_split() {
                Ok(Some(aref)) if aref.inner.is_empty() => aref.outer.parent(),
                _ => dir.parent(),
            };
            if let Some(parent) = parent {
                cd_outcome = cd(app, backend, events, parent).await;
            } else {
                // Raíz `/` o raíz de unidad Windows (`parent()` = None): antes
                // era un no-op SILENCIOSO (#20). Ahora avisa por la barra.
                app.message = Some(t("msg-nav-at-top"));
            }
        }
        "pane.copy" | "pane.move" => {
            let kind = if cmd == "pane.copy" {
                TransferKind::Copy
            } else {
                TransferKind::Move
            };
            // Destino ortodoxo: el dir del OTRO pane + el mismo nombre.
            let other = &app.panes[1 - app.focus()];
            let target = app.focused().selected().and_then(|e| {
                let name = e.path.file_name()?.clone();
                Some((e.path.clone(), other.dir.join(name)))
            });
            if let Some((from, to)) = target {
                app.modal = Some(Modal::ConfirmTransfer { kind, from, to });
            }
        }
        "pane.delete" | "pane.delete-permanent" => {
            if let Some(e) = app.focused().selected() {
                // F8 = papelera si el provider la declara; sin ella, el
                // MISMO diálogo avisa de PERMANENTE (degradación con
                // usuario informado, ADR 0009). shift+f8 = permanente.
                let hay_papelera = backend
                    .capabilities(&e.path)
                    .await
                    .is_ok_and(|c| c.flags.contains(norte_proto::CapabilityFlags::TRASH));
                let permanent = cmd == "pane.delete-permanent" || !hay_papelera;
                app.modal = Some(Modal::ConfirmDelete {
                    target: e.path.clone(),
                    permanent,
                });
            }
        }
        "pane.view" => {
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
        "viewer.close" => app.viewer = None,
        "viewer.up" => viewer_do(app, |v| v.scroll_up(1)),
        "viewer.down" => viewer_do(app, |v| v.scroll_down(1)),
        "viewer.page-up" => viewer_do(app, |v| v.scroll_up(norte_tui::viewer::PAGE)),
        "viewer.page-down" => viewer_do(app, |v| v.scroll_down(norte_tui::viewer::PAGE)),
        "viewer.top" => viewer_do(app, norte_tui::viewer::Viewer::scroll_top),
        "viewer.bottom" => viewer_do(app, norte_tui::viewer::Viewer::scroll_bottom),
        "viewer.encoding" => viewer_do(app, norte_tui::viewer::Viewer::cycle_encoding),
        "viewer.encoding-auto" => viewer_do(app, norte_tui::viewer::Viewer::reset_encoding),
        "viewer.hex" => viewer_do(app, norte_tui::viewer::Viewer::toggle_hex),
        "app.help" => {
            app.help = Some(Help {
                lines: help_lines.to_vec(),
                scroll: 0,
            });
        }
        "app.theme" => app.open_theme_picker(),
        "app.extensions" => match backend.plugins_list().await {
            // El catálogo llega YA ordenado por categoría e id desde el core.
            Ok(list) => {
                app.extensions = Some(ExtensionManager {
                    plugins: list.plugins,
                    errors: list.errors,
                    cursor: 0,
                });
            }
            Err(e) => app.message = Some(error_message(&e)),
        },
        "task.cancel" => {
            app.message = Some(if app.board.cancel_last_running() {
                t("msg-cancelling")
            } else {
                t("msg-no-tasks")
            });
        }
        // Inalcanzable: todo keymap se valida contra COMMANDS al cargar
        // (y COMMANDS vive en la lib: una sola fuente).
        _ => debug_assert!(false, "comando validado sin brazo: {cmd}"),
    }
    cd_outcome
}

fn viewer_do(app: &mut App, f: impl FnOnce(&mut Viewer)) {
    if let Some(v) = &mut app.viewer {
        f(v);
    }
}

/// Presupuesto de lectura del viewer: cabecera de 256 KiB (el resto del
/// archivo NO se lee — rango de ADR 0005; «cargar más» = deuda de M2).
/// OJO si esto crece (>~1 MiB): `Viewer::recompute` y `rows()` corren en
/// el hilo del loop — harían falta `spawn_blocking` + índice de líneas.
const VIEW_CAP: u64 = 256 * 1024;

/// Abre el viewer leyendo la CABECERA vía el core (regla 7), cancelable
/// como el cd (Esc abandona, Ctrl-C sale).
async fn open_viewer(app: &mut App, backend: &Backend, events: &mut EventStream, path: VPath) {
    // Fase 1 leer la cabecera, fase 2 (M4-P5) intentar el preview de un plugin.
    // Ambas van dentro de la MISMA future para que Esc/Ctrl-C cancelen en
    // cualquiera de las dos. Un fallo del preview NUNCA impide ver el crudo.
    let fut = async {
        let (bytes, truncated) = read_head(backend, &path).await?;
        let viewer = match backend.plugin_preview(&path).await {
            Ok(res) => match res.preview {
                Some(p) => Viewer::with_plugin_preview(path.clone(), p.plugin_name, &p.output),
                None => Viewer::new(path.clone(), bytes, truncated),
            },
            // Un plugin roto no bloquea el archivo: vista cruda de siempre.
            Err(_) => Viewer::new(path.clone(), bytes, truncated),
        };
        Ok::<Viewer, Error>(viewer)
    };
    tokio::pin!(fut);
    loop {
        tokio::select! {
            res = &mut fut => {
                match res {
                    Ok(viewer) => app.viewer = Some(viewer),
                    Err(e) => app.message = Some(ta("msg-view-error", &[("error", &error_category(&e))])),
                }
                return;
            }
            maybe = events.next() => {
                match maybe {
                    Some(Ok(Event::Key(key)))
                        if key.kind == crossterm::event::KeyEventKind::Press =>
                    {
                        match (key.code, key.modifiers) {
                            (KeyCode::Char('c'), m) if m.contains(KeyModifiers::CONTROL) => {
                                app.quit = true;
                                return;
                            }
                            (KeyCode::Esc, _) => return,
                            _ => {}
                        }
                    }
                    Some(Ok(_)) => {}
                    Some(Err(_)) | None => return,
                }
            }
        }
    }
}

/// Lee hasta `VIEW_CAP + 1` bytes: el byte extra delata el truncado.
async fn read_head(backend: &Backend, path: &VPath) -> Result<(Vec<u8>, bool), Error> {
    let mut out = backend
        .read(
            path,
            Some(norte_proto::ByteRange {
                offset: 0,
                len: Some(VIEW_CAP + 1),
            }),
        )
        .await?;
    let truncated = out.len() as u64 > VIEW_CAP;
    if truncated {
        out.truncate(usize::try_from(VIEW_CAP).unwrap_or(usize::MAX));
    }
    Ok((out, truncated))
}

/// Si la entrada es un contenedor navegable (`.<formato>` de la whitelist
/// de proto, extensión ASCII case-insensitive), la raíz de su interior
/// (ADR 0018). El mapa extensión→formato es azúcar de presentación; la
/// validación real es del core. Un SYMLINK a un archivo no entra como
/// contenedor en v1 (decisión consciente: exigiría resolver el target por
/// stat del core; issue de fase 8g).
fn archive_root_for(e: &norte_proto::Entry) -> Option<VPath> {
    fn ends_ci(name: &[u8], suffix: &[u8]) -> bool {
        name.len() >= suffix.len() && name[name.len() - suffix.len()..].eq_ignore_ascii_case(suffix)
    }
    if e.kind != EntryKind::File {
        return None;
    }
    let name = e.path.file_name()?.as_bytes();
    let format = norte_proto::ARCHIVE_FORMATS
        .iter()
        .find(|f| ends_ci(name, format!(".{f}").as_bytes()))?;
    // Falla (exterior con `!`, ya compuesto…): no es navegable — Enter no-op.
    VPath::archive_compose(format, &e.path, &[]).ok()
}

/// Listado COMPLETO y ordenado de `dir` (para `refresh_panes` tras una
/// mutación: conserva el cursor por índice). Una entrada con error corta el
/// listado — mejor un error honesto que un listado silenciosamente incompleto.
async fn listing(backend: &Backend, dir: &VPath) -> Result<Vec<Entry>, Error> {
    let mut entries = backend.list(dir).await?;
    sort_entries(&mut entries);
    Ok(entries)
}

/// Primera página de `dir` (hasta [`FIRST_PAGE`]) más el stream con el RESTO
/// (o `None` si el dir cabía en la primera página). El primer render no espera
/// al listado entero (ADR 0017). Regla 7: el TUI no toca el FS.
async fn first_page(
    backend: &Backend,
    dir: &VPath,
) -> Result<(Vec<Entry>, Option<EntryStream>), Error> {
    let mut stream = backend.list_stream(dir).await?;
    let mut first = Vec::with_capacity(FIRST_PAGE);
    while first.len() < FIRST_PAGE {
        match stream.next().await {
            Some(item) => first.push(item?),
            // El dir cabía en la primera página: no hay resto que drenar.
            None => return Ok((first, None)),
        }
    }
    Ok((first, Some(stream)))
}

/// Arranca el drenador del RESTO del listado: envía lotes coalescidos al run
/// loop, que los aplica con [`Pane::extend_listing`]. Soltar el `rx` (un cd
/// nuevo) mata el drenador en su próximo envío → suelta el stream (regla 3).
fn spawn_fill(pane: usize, mut stream: EntryStream) -> Fill {
    // Bounded a 1: el drenador no corre por delante del run loop más de un
    // lote (backpressure); el pico de memoria es un lote, no todo el dir.
    let (tx, rx) = tokio::sync::mpsc::channel::<FillMsg>(1);
    tokio::spawn(async move {
        let mut batch = Vec::with_capacity(FILL_BATCH);
        let mut flush = tokio::time::interval(FILL_INTERVAL);
        flush.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        flush.tick().await; // consume el tick inmediato del interval
        loop {
            tokio::select! {
                item = stream.next() => match item {
                    Some(Ok(e)) => {
                        batch.push(e);
                        if batch.len() >= FILL_BATCH
                            && tx
                                .send(FillMsg::Batch(std::mem::take(&mut batch)))
                                .await
                                .is_err()
                        {
                            return; // el run loop soltó el rx (cd nuevo)
                        }
                    }
                    Some(Err(_)) => {
                        let _ = tx.send(FillMsg::Failed).await;
                        return;
                    }
                    None => {
                        if !batch.is_empty() {
                            let _ = tx.send(FillMsg::Batch(batch)).await;
                        }
                        return; // fin: drop(tx) cierra el canal → finish_listing
                    }
                },
                _ = flush.tick() => {
                    // Vacía un lote PARCIAL (progreso en streams lentos).
                    if !batch.is_empty()
                        && tx
                            .send(FillMsg::Batch(std::mem::take(&mut batch)))
                            .await
                            .is_err()
                    {
                        return;
                    }
                }
            }
        }
    });
    Fill { pane, rx }
}

/// cd CANCELABLE (regla 3): el listado corre contra el stream de eventos —
/// Esc lo abandona (el pane se queda donde estaba) y Ctrl-C sale del TUI
/// (atajos FIJOS durante un cd: aquí no aplica el keymap — son la salida de
/// emergencia y no deben ser remapeables a algo que no exista). Soltar el
/// future del listado detiene al productor del provider (testeado en
/// vfs-local). El resto de teclas se descartan mientras dura el cd.
async fn cd(app: &mut App, backend: &Backend, events: &mut EventStream, dir: VPath) -> Cd {
    let fut = first_page(backend, &dir);
    tokio::pin!(fut);
    loop {
        tokio::select! {
            res = &mut fut => {
                match res {
                    Ok((mut first, stream)) => {
                        sort_entries(&mut first);
                        let more = stream.is_some();
                        app.focused_mut().begin_listing(dir.clone(), first, more);
                        let pane = app.focus();
                        // Si queda stream, un drenador lo rellena en background.
                        return match stream {
                            Some(s) => Cd::Filling(spawn_fill(pane, s)),
                            None => Cd::Replaced(pane),
                        };
                    }
                    // Primer contacto TOFU (#45): en vez de una línea de
                    // error con la huella, abre el modal de confianza — `y`
                    // confía y REINTENTA esta misma navegación.
                    Err(Error::HostKeyUnknown {
                        host,
                        port,
                        algo,
                        fingerprint,
                    }) => {
                        app.modal = Some(Modal::TrustHostKey {
                            host,
                            port,
                            algo,
                            fingerprint,
                            dir: dir.clone(),
                        });
                        // El pane NO se tocó (solo se abrió el modal): Cancelled
                        // conserva un relleno en vuelo del listado anterior, que
                        // sigue siendo válido (MINOR del rust-reviewer).
                        return Cd::Cancelled;
                    }
                    // Un error de listado NO tumba el TUI: el pane se queda,
                    // pero un relleno previo de ESTE pane ya no aplica.
                    Err(e) => {
                        app.message = Some(error_message(&e));
                        return Cd::Replaced(app.focus());
                    }
                }
            }
            maybe = events.next() => {
                match maybe {
                    Some(Ok(Event::Key(key)))
                        if key.kind == crossterm::event::KeyEventKind::Press =>
                    {
                        match (key.code, key.modifiers) {
                        (KeyCode::Char('c'), m) if m.contains(KeyModifiers::CONTROL) => {
                            app.quit = true;
                            return Cd::Cancelled;
                        }
                            (KeyCode::Esc, _) => return Cd::Cancelled,
                            _ => {}
                        }
                    }
                    Some(Ok(_)) => {}
                    Some(Err(_)) | None => return Cd::Cancelled,
                }
            }
        }
    }
}

#[cfg(test)]
mod archive_nav_tests {
    use super::*;
    use norte_proto::{Entry, EntryKind};

    fn entry(wire: &str, kind: EntryKind) -> Entry {
        Entry {
            path: VPath::parse(wire).expect("wire de test"),
            kind,
            size: None,
            mtime_ms: None,
        }
    }

    #[test]
    fn archive_root_for_decide_por_extension_y_kind() {
        let e = entry("file:///d/A.ZIP", EntryKind::File);
        assert_eq!(
            archive_root_for(&e).expect("mayúsculas entran").to_wire(),
            "zip+file:///d/A.ZIP/!"
        );
        assert!(archive_root_for(&entry("file:///d/a.tar", EntryKind::File)).is_some());
        assert!(archive_root_for(&entry("file:///d/a.txt", EntryKind::File)).is_none());
        // Un dir llamado x.zip NO es contenedor; un symlink tampoco (v1).
        assert!(archive_root_for(&entry("file:///d/x.zip", EntryKind::Dir)).is_none());
        assert!(archive_root_for(&entry("file:///d/x.zip", EntryKind::Symlink)).is_none());
        // Ya compuesto (zip dentro de tar): v1 sin anidar → no-op.
        assert!(archive_root_for(&entry("tar+file:///a.tar/!/i.zip", EntryKind::File)).is_none());
    }
}
