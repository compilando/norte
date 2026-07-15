//! Binario del TUI (fases 3–4 M1): loop de eventos async sobre el core
//! EMBEBIDO o contra el DAEMON (fase 3 M2, por `[daemon] mode` o
//! `--daemon`), con keymap engine (ADR 0006). Regla 7: solo cambia el
//! transporte.
//! `ratatui::init/restore` gestionan raw mode + pantalla alternativa con
//! hook de pánico incluido: la terminal del usuario JAMÁS queda rota.
#![forbid(unsafe_code)]

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
    App, DialogOutcome, Help, Modal, Pane, TransferKind, dialog_key, sort_entries,
};
use norte_tui::config::{self, Layers, WatchMode};
use norte_tui::keymap::{COMMANDS, Chord, Effective, Resolution, Resolver, Screen, presets};
use norte_tui::tasks::RetrySpec;
use norte_tui::ui;
use norte_tui::viewer::Viewer;
use norte_vfs_local::LocalProvider;

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
/// keymap (ADR 0007) para las DOS pantallas (browse y viewer).
fn build_keymaps(
    cfg: &config::LoadedConfig,
    cli_preset: Option<&str>,
) -> Result<(Effective, Effective)> {
    let preset_name = cli_preset.unwrap_or(&cfg.preset);
    let presets = presets();
    let (_, preset) = presets
        .iter()
        .find(|(n, _)| *n == preset_name)
        .with_context(|| {
            let nombres: Vec<&str> = presets.iter().map(|(n, _)| *n).collect();
            format!("preset desconocido {preset_name:?}; disponibles: {nombres:?}")
        })?;
    let browse = Effective::build_for(preset, &cfg.keymap_layers, COMMANDS, Screen::Browse)
        .context("keymap inválido")?;
    let viewer = Effective::build_for(preset, &cfg.keymap_layers, COMMANDS, Screen::Viewer)
        .context("keymap inválido")?;
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
    loop {
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
            }
            maybe = events.next() => {
                let Some(event) = maybe else { return Ok(()); };
                if let Event::Key(key) = event.context("evento de terminal")?
                    && key.kind == crossterm::event::KeyEventKind::Press
                {
                    app.message = None;
                    if let Some(help) = &mut app.help {
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
                        on_dialog_key(app, backend, key.code).await;
                    } else {
                        // Pantalla activa: el viewer tiene su contexto.
                        let active = if app.viewer.is_some() {
                            &mut *viewer_resolver
                        } else {
                            &mut *resolver
                        };
                        match active.push(Chord::from_event(key.modifiers, key.code)) {
                            Resolution::Run(cmd) => {
                                app.pending.clear();
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
            app.message = Some(e);
        }
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
                    &[("error", &format!("{e:#}"))],
                ));
            }
        },
        Err(e) => app.message = Some(ta("msg-config-not-applied", &[("error", &e.to_string())])),
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
        app.open_next_collision();
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
                    // Render por CATEGORÍA (spec §17.7): Display estable,
                    // jamás strings del OS.
                    app.message = Some(ta("msg-error", &[("error", &error.to_string())]));
                    refresh = true;
                }
            }
            _ => {}
        }
    }
    app.open_next_collision();
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
                        Err(e) => app.message = Some(ta("msg-refresh-error", &[("error", &e.to_string())])),
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

/// Teclas de un modal abierto (hardcodeadas, issue #24).
async fn on_dialog_key(app: &mut App, backend: &Backend, code: KeyCode) {
    let Some(modal) = app.modal.clone() else {
        return;
    };
    match dialog_key(&modal, code) {
        DialogOutcome::Open => {}
        DialogOutcome::Cancelled => {
            app.modal = None;
            app.open_next_collision();
        }
        DialogOutcome::Confirmed => {
            app.modal = None;
            app.open_next_collision();
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
                        Err(e) => app.message = Some(ta("msg-error", &[("error", &e.to_string())])),
                    }
                }
                Modal::ConfirmTransfer { kind, from, to } => {
                    submit_transfer(app, backend, kind, from, to, TransferOptions::default()).await;
                }
                Modal::Collision { .. } => {}
            }
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
            app.open_next_collision();
        }
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
        Err(e) => app.message = Some(ta("msg-error", &[("error", &e.to_string())])),
    }
}

/// Ejecuta un comando nombrado (ADR 0006: los mismos nombres que verán la
/// palette y el wire). Un error de listado en un cd NO tumba el TUI: el
/// pane se queda donde estaba (aviso visible: barra de mensajes, issue #20).
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
    let fut = read_head(backend, &path);
    tokio::pin!(fut);
    loop {
        tokio::select! {
            res = &mut fut => {
                match res {
                    Ok((bytes, truncated)) => {
                        app.viewer = Some(Viewer::new(path.clone(), bytes, truncated));
                    }
                    Err(e) => app.message = Some(ta("msg-view-error", &[("error", &e.to_string())])),
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
                    // Un error de listado NO tumba el TUI: el pane se queda,
                    // pero un relleno previo de ESTE pane ya no aplica.
                    Err(e) => {
                        app.message = Some(ta("msg-error", &[("error", &e.to_string())]));
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
