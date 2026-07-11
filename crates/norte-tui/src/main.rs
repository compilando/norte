//! Binario del TUI (fase 3 M1): loop de eventos async sobre el core
//! EMBEBIDO (el daemon llega en M2). `ratatui::init/restore` gestionan raw
//! mode + pantalla alternativa con hook de pánico incluido: la terminal del
//! usuario JAMÁS queda rota.
#![forbid(unsafe_code)]

use std::sync::Arc;

use anyhow::{Context, Result};
use crossterm::event::{Event, EventStream};
use futures::StreamExt;
use norte_core::Engine;
use norte_proto::{Entry, EntryKind, Error, VPath};
use norte_tui::app::{App, Pane, sort_entries};
use norte_tui::keys::{Action, action_for};
use norte_tui::ui;
use norte_vfs_local::LocalProvider;

#[tokio::main]
async fn main() -> Result<()> {
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

    let mut terminal = ratatui::init();
    let res = run(&mut terminal, &mut app, &engine).await;
    ratatui::restore();
    res
}

async fn run(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    engine: &Engine,
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
        if let Event::Key(key) = event.context("evento de terminal")? {
            handle_action(app, engine, &mut events, action_for(key)).await;
        }
        // Resize/Focus/etc: el draw del inicio del loop repinta solo.
    }
}

/// Un error de listado en un cd NO tumba el TUI: el pane se queda donde
/// estaba (el aviso visible llega con la barra de mensajes, issue #20).
async fn handle_action(app: &mut App, engine: &Engine, events: &mut EventStream, action: Action) {
    match action {
        Action::Quit => app.quit = true,
        Action::SwitchFocus => app.switch_focus(),
        Action::MoveUp(n) => app.focused_mut().move_up(n),
        Action::MoveDown(n) => app.focused_mut().move_down(n),
        Action::Start => app.focused_mut().move_to_start(),
        Action::End => app.focused_mut().move_to_end(),
        Action::Enter => {
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
        Action::Parent => {
            if let Some(parent) = app.focused().dir.parent() {
                cd(app, engine, events, parent).await;
            }
        }
        Action::None => {}
    }
}

/// cd CANCELABLE (regla 3): el listado corre contra el stream de eventos —
/// Esc lo abandona (el pane se queda donde estaba), Ctrl-C/q salen del TUI.
/// Soltar el future del listado suelta el stream del provider, que detiene
/// a su productor (testeado en vfs-local). El resto de teclas se descartan
/// mientras dura el cd.
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
                    Some(Ok(Event::Key(key))) => match action_for(key) {
                        Action::Quit => {
                            app.quit = true;
                            return;
                        }
                        _ if key.code == crossterm::event::KeyCode::Esc => return,
                        _ => {}
                    },
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
