//! Binario del TUI (fases 3–4 M1): loop de eventos async sobre el core
//! EMBEBIDO (el daemon llega en M2), con keymap engine (ADR 0006).
//! `ratatui::init/restore` gestionan raw mode + pantalla alternativa con
//! hook de pánico incluido: la terminal del usuario JAMÁS queda rota.
#![forbid(unsafe_code)]

use std::sync::Arc;

use anyhow::{Context, Result};
use crossterm::event::{Event, EventStream, KeyCode, KeyModifiers};
use futures::StreamExt;
use norte_core::Engine;
use norte_proto::{Entry, EntryKind, Error, VPath};
use norte_tui::app::{App, Pane, sort_entries};
use norte_tui::keymap::{COMMANDS, Chord, Effective, Resolution, Resolver, presets};
use norte_tui::ui;
use norte_vfs_local::LocalProvider;

/// Filas que salta `cursor.page-up/down` (fijo hasta que el alto real del
/// pane viaje con el comando).
const PAGE: usize = 10;

#[tokio::main]
async fn main() -> Result<()> {
    // Preset por argumento (`norte-tui vim`); default orthodox (decisión
    // 2026-07-10). La capa de usuario (keymap.toml en disco) llega con la
    // config en capas de la fase 6.
    let preset_name = std::env::args_os().nth(1).map_or_else(
        || "orthodox".to_owned(),
        |s| s.to_string_lossy().into_owned(),
    );
    let presets = presets();
    let (_, preset) = presets
        .iter()
        .find(|(n, _)| *n == preset_name)
        .with_context(|| {
            let nombres: Vec<&str> = presets.iter().map(|(n, _)| *n).collect();
            format!("preset desconocido {preset_name:?}; disponibles: {nombres:?}")
        })?;
    let eff = Effective::build(preset, None, COMMANDS).context("keymap inválido")?;

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
    let mut resolver = Resolver::new(&eff);

    let mut terminal = ratatui::init();
    let res = run(&mut terminal, &mut app, &engine, &mut resolver).await;
    ratatui::restore();
    res
}

async fn run(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    engine: &Engine,
    resolver: &mut Resolver<'_>,
) -> Result<()> {
    let mut events = EventStream::new();
    loop {
        // Exención puntual de la regla 2: el draw escribe stdout síncrono
        // (patrón async oficial de ratatui; acotado, runtime multi-thread).
        terminal.draw(|f| ui::draw(f, app))?;
        if app.quit {
            return Ok(());
        }
        let Some(event) = events.next().await else {
            return Ok(());
        };
        if let Event::Key(key) = event.context("evento de terminal")?
            && key.kind == crossterm::event::KeyEventKind::Press
        {
            match resolver.push(Chord::from_event(key.modifiers, key.code)) {
                Resolution::Run(cmd) => {
                    app.pending.clear();
                    dispatch(app, engine, &mut events, &cmd).await;
                }
                Resolution::Pending(_) => {
                    app.pending = resolver
                        .pending()
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(" ");
                }
                Resolution::Reset => app.pending.clear(),
            }
        }
        // Resize/Focus/etc: el draw del inicio del loop repinta solo.
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
        // Inalcanzable: todo keymap se valida contra COMMANDS al cargar
        // (y COMMANDS vive en la lib: una sola fuente).
        _ => debug_assert!(false, "comando validado sin brazo: {cmd}"),
    }
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
