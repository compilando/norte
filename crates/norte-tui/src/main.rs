//! Binario del TUI (fases 3–4 M1): loop de eventos async sobre el core
//! EMBEBIDO (el daemon llega en M2), con keymap engine (ADR 0006).
//! `ratatui::init/restore` gestionan raw mode + pantalla alternativa con
//! hook de pánico incluido: la terminal del usuario JAMÁS queda rota.
#![forbid(unsafe_code)]

use std::sync::Arc;

use anyhow::{Context, Result};
use crossterm::event::{Event, EventStream, KeyCode, KeyModifiers};
use futures::StreamExt;
use norte_core::{Engine, TransferOptions};
use norte_proto::DeleteMode;
use norte_proto::{Entry, EntryKind, Error, VPath};
use norte_tui::app::{App, DialogOutcome, Modal, Pane, TransferKind, dialog_key, sort_entries};
use norte_tui::config::{self, Layers, WatchMode};
use norte_tui::keymap::{COMMANDS, Chord, Effective, Resolution, Resolver, Screen, presets};
use norte_tui::tasks::RetrySpec;
use norte_tui::ui;
use norte_tui::viewer::Viewer;
use norte_vfs_local::LocalProvider;

/// Filas que salta `cursor.page-up/down` (fijo hasta que el alto real del
/// pane viaje con el comando).
const PAGE: usize = 10;

#[tokio::main]
async fn main() -> Result<()> {
    // Capas de config (ADR 0007) + flag: el argumento (`norte-tui vim`) es
    // la capa MÁS alta y pisa el preset de norte.toml.
    let cli_preset = std::env::args_os()
        .nth(1)
        .map(|s| s.to_string_lossy().into_owned());
    let layers = config::standard_layers();
    let cfg = config::load_async(layers.clone())
        .await
        .context("config inválida")?;
    let (browse_eff, viewer_eff) = build_keymaps(&cfg, cli_preset.as_deref())?;

    let engine = Engine::new();
    engine.register_provider(Arc::new(LocalProvider::os_root()));

    let cwd = std::env::current_dir().context("cwd")?;
    // Deuda conocida: en Windows un cwd UNC (\\server\share) no es
    // representable todavía y esto aborta con error claro (issue #22).
    let start = norte_vfs_local::vpath_from_native(&cwd)
        .map_err(|e| anyhow::anyhow!("cwd no representable como VPath: {e}"))?;
    let left = Pane::new(start.clone(), listing(&engine, &start).await?);
    let right = Pane::new(start.clone(), listing(&engine, &start).await?);
    let mut app = App::new(left, right);
    let mut resolver = Resolver::new(browse_eff);
    let mut viewer_resolver = Resolver::new(viewer_eff);

    // Hot-reload: vigilancia de las capas, con aviso si degrada a polling.
    let (cfg_tx, cfg_rx) = tokio::sync::mpsc::channel(8);
    let watch = config::watch(&layers, cfg_tx).await;
    if watch.mode == WatchMode::Polling {
        app.message = Some("config: vigilancia degradada a polling".to_owned());
    }

    let mut terminal = ratatui::init();
    let res = run(
        &mut terminal,
        &mut app,
        &engine,
        &mut resolver,
        &mut viewer_resolver,
        layers,
        cli_preset,
        cfg_rx,
    )
    .await;
    ratatui::restore();
    drop(watch);
    res
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

#[allow(clippy::too_many_arguments)] // wiring del binario, no API
async fn run(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    engine: &Engine,
    resolver: &mut Resolver,
    viewer_resolver: &mut Resolver,
    layers: Layers,
    cli_preset: Option<String>,
    mut cfg_rx: tokio::sync::mpsc::Receiver<()>,
) -> Result<()> {
    let mut events = EventStream::new();
    // Tick del panel de tasks: copia snapshots del watch (jamás bloquea).
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(100));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // Debounce del hot-reload SIN bloquear el loop (revisión fase 6): cada
    // evento de config empuja el deadline; el reload corre cuando vence.
    let mut reload_at: Option<tokio::time::Instant> = None;
    loop {
        // Exención puntual de la regla 2: el draw escribe stdout síncrono
        // (patrón async oficial de ratatui; acotado, runtime multi-thread).
        terminal.draw(|f| ui::draw(f, app))?;
        if app.quit {
            return Ok(());
        }
        tokio::select! {
            _ = tick.tick() => {
                on_tick(app, engine, &mut events).await;
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
                reload_config(app, resolver, viewer_resolver, &layers, cli_preset.as_deref())
                    .await;
            }
            maybe = events.next() => {
                let Some(event) = maybe else { return Ok(()); };
                if let Event::Key(key) = event.context("evento de terminal")?
                    && key.kind == crossterm::event::KeyEventKind::Press
                {
                    app.message = None;
                    if app.modal.is_some() {
                        on_dialog_key(app, engine, key.code);
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
                                dispatch(app, engine, &mut events, &cmd).await;
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
async fn reload_config(
    app: &mut App,
    resolver: &mut Resolver,
    viewer_resolver: &mut Resolver,
    layers: &Layers,
    cli_preset: Option<&str>,
) {
    match config::load_async(layers.clone()).await {
        Ok(cfg) => match build_keymaps(&cfg, cli_preset) {
            Ok((browse, viewer)) => {
                *resolver = Resolver::new(browse);
                *viewer_resolver = Resolver::new(viewer);
                app.pending.clear();
                app.message = Some("config recargada".to_owned());
            }
            Err(e) => app.message = Some(format!("config NO aplicada: {e:#}")),
        },
        Err(e) => app.message = Some(format!("config NO aplicada: {e}")),
    }
}

/// Tick: refresca snapshots del panel y reacciona a las tasks que ACABAN
/// de terminar — colisión con contexto → a la COLA de diálogos (jamás se
/// pisa un modal abierto, hallazgo B1); el resto → mensaje por categoría +
/// refresh de ambos panes (una mutación pudo cambiarlos).
/// (Strings de mensaje hardcodeados hasta Fluent — fase 9, issue #1.)
async fn on_tick(app: &mut App, engine: &Engine, events: &mut EventStream) {
    let finished = app.board.tick();
    if finished.is_empty() {
        app.open_next_collision();
        return;
    }
    let mut refresh = false;
    for fin in finished {
        use norte_proto::TaskState;
        match fin.state {
            TaskState::Completed => {
                refresh = true;
                app.message = Some("hecho".to_owned());
            }
            TaskState::Cancelled => {
                refresh = true;
                app.message = Some("cancelado".to_owned());
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
                        app.message =
                            Some("sin papelera aquí: F8 de nuevo para permanente".to_owned());
                    }
                } else if let (Error::Conflict { .. }, Some(retry)) = (&error, fin.retry) {
                    app.pending_collisions.push_back(retry);
                } else {
                    // Render por CATEGORÍA (spec §17.7): Display estable,
                    // jamás strings del OS.
                    app.message = Some(format!("error: {error}"));
                    refresh = true;
                }
            }
            _ => {}
        }
    }
    app.open_next_collision();
    if refresh {
        refresh_panes(app, engine, events).await;
    }
}

/// Recarga ambos panes tras una mutación (pueden mostrar el mismo dir).
/// CANCELABLE como el cd (regla 3): Esc abandona el refresh (los panes se
/// quedan como estaban), Ctrl-C sale. El cursor se conserva por ÍNDICE
/// (tras un delete queda en la siguiente entrada — semántica ortodoxa).
async fn refresh_panes(app: &mut App, engine: &Engine, events: &mut EventStream) {
    for i in 0..app.panes.len() {
        let dir = app.panes[i].dir.clone();
        let fut = listing(engine, &dir);
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
                        }
                        // Sin silencio: el dir pudo desaparecer (issue #20).
                        Err(e) => app.message = Some(format!("refresh: {e}")),
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
fn on_dialog_key(app: &mut App, engine: &Engine, code: KeyCode) {
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
                    match engine.delete_with(&target, mode) {
                        Ok(handle) => {
                            app.board
                                .push_full(handle, None, (!permanent).then(|| target.clone()));
                        }
                        Err(e) => app.message = Some(format!("error: {e}")),
                    }
                }
                Modal::ConfirmTransfer { kind, from, to } => {
                    submit_transfer(app, engine, kind, from, to, TransferOptions::default());
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
                submit_transfer(app, engine, retry.kind, retry.from, retry.to, opts);
            }
            app.open_next_collision();
        }
    }
}

/// Encola una transferencia y la registra en el panel con su contexto de
/// reintento (para el diálogo de colisión).
fn submit_transfer(
    app: &mut App,
    engine: &Engine,
    kind: TransferKind,
    from: VPath,
    to: VPath,
    opts: TransferOptions,
) {
    let res = match kind {
        TransferKind::Copy => engine.copy_with(&from, &to, opts),
        TransferKind::Move => engine.move_with(&from, &to, opts),
    };
    match res {
        Ok(handle) => app.board.push(
            handle,
            Some(RetrySpec {
                kind,
                from,
                to,
                opts,
            }),
        ),
        Err(e) => app.message = Some(format!("error: {e}")),
    }
}

/// Ejecuta un comando nombrado (ADR 0006: los mismos nombres que verán la
/// palette y el wire). Un error de listado en un cd NO tumba el TUI: el
/// pane se queda donde estaba (aviso visible: barra de mensajes, issue #20).
async fn dispatch(app: &mut App, engine: &Engine, events: &mut EventStream, cmd: &str) {
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
            // core, no el TUI (regla 7).
            let target = app
                .focused()
                .selected()
                .filter(|e| matches!(e.kind, EntryKind::Dir | EntryKind::Symlink))
                .map(|e| e.path.clone());
            if let Some(dir) = target {
                cd(app, engine, events, dir).await;
            }
        }
        "nav.parent" => {
            if let Some(parent) = app.focused().dir.parent() {
                cd(app, engine, events, parent).await;
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
                let hay_papelera = engine
                    .capabilities(&e.path)
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
                open_viewer(app, engine, events, path).await;
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
        "task.cancel" => {
            app.message = Some(if app.board.cancel_last_running() {
                "cancelando…".to_owned()
            } else {
                "no hay tasks en marcha".to_owned()
            });
        }
        // Inalcanzable: todo keymap se valida contra COMMANDS al cargar
        // (y COMMANDS vive en la lib: una sola fuente).
        _ => debug_assert!(false, "comando validado sin brazo: {cmd}"),
    }
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
async fn open_viewer(app: &mut App, engine: &Engine, events: &mut EventStream, path: VPath) {
    let fut = read_head(engine, &path);
    tokio::pin!(fut);
    loop {
        tokio::select! {
            res = &mut fut => {
                match res {
                    Ok((bytes, truncated)) => {
                        app.viewer = Some(Viewer::new(path.clone(), bytes, truncated));
                    }
                    Err(e) => app.message = Some(format!("view: {e}")),
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
async fn read_head(engine: &Engine, path: &VPath) -> Result<(Vec<u8>, bool), Error> {
    let mut stream = engine
        .read(
            path,
            Some(norte_proto::ByteRange {
                offset: 0,
                len: Some(VIEW_CAP + 1),
            }),
        )
        .await?;
    let mut out = Vec::new();
    while let Some(chunk) = stream.next().await {
        out.extend_from_slice(&chunk?);
    }
    let truncated = out.len() as u64 > VIEW_CAP;
    if truncated {
        out.truncate(usize::try_from(VIEW_CAP).unwrap_or(usize::MAX));
    }
    Ok((out, truncated))
}

/// cd CANCELABLE (regla 3): el listado corre contra el stream de eventos —
/// Esc lo abandona (el pane se queda donde estaba) y Ctrl-C sale del TUI
/// (atajos FIJOS durante un cd: aquí no aplica el keymap — son la salida de
/// emergencia y no deben ser remapeables a algo que no exista). Soltar el
/// future del listado detiene al productor del provider (testeado en
/// vfs-local). El resto de teclas se descartan mientras dura el cd.
async fn cd(app: &mut App, engine: &Engine, events: &mut EventStream, dir: VPath) {
    let fut = listing(engine, &dir);
    tokio::pin!(fut);
    loop {
        tokio::select! {
            res = &mut fut => {
                if let Ok(entries) = res {
                    app.focused_mut().set_listing(dir.clone(), entries);
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

/// Listado completo y ordenado de `dir` vía el core (regla 7: el TUI no
/// toca el FS). Una entrada con error corta el listado: mejor un error
/// honesto que un listado silenciosamente incompleto.
async fn listing(engine: &Engine, dir: &VPath) -> Result<Vec<Entry>, Error> {
    let mut stream = engine.list(dir).await?;
    let mut entries = Vec::new();
    while let Some(item) = stream.next().await {
        entries.push(item?);
    }
    sort_entries(&mut entries);
    Ok(entries)
}
