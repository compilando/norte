//! The renderer's binary: mounts the host, opens ONE window and pumps.
#![forbid(unsafe_code)]

use std::process::ExitCode;

use norte_gui_tauri::commands::{AppState, Bridge};
use norte_gui_tauri::sink::{EVENT_CATALOG, EVENT_LAGGED, EVENT_UPDATE, SinkError, UpdateSink};
use norte_gui_tauri::startup::{self, USAGE};
use norte_ui_host::{BridgeEnvelope, UiAction, dto::UiUpdate};
use tauri::{Emitter, Manager};

/// When the process started. It is the cold-start reference: what gets
/// measured runs from here to when the renderer asks for its first frame,
/// which is when the webview exists, has loaded its script and can paint.
static STARTUP: std::sync::LazyLock<std::time::Instant> =
    std::sync::LazyLock::new(std::time::Instant::now);

/// The real sink: the main window.
struct WindowSink {
    app: tauri::AppHandle,
}

impl UpdateSink for WindowSink {
    fn update(&self, env: &BridgeEnvelope<UiUpdate>) -> Result<(), SinkError> {
        self.app
            .emit(EVENT_UPDATE, env)
            .map_err(|e| SinkError(e.to_string()))
    }

    fn lagged(&self) -> Result<(), SinkError> {
        self.app
            .emit(EVENT_LAGGED, ())
            .map_err(|e| SinkError(e.to_string()))
    }
}

// `tauri::State` travels BY VALUE in a command: that is what the macro
// generates, and there is no by-reference version. The lint does not know
// about that restriction.
#[expect(
    clippy::needless_pass_by_value,
    reason = "`tauri::command` requires `State` by value"
)]
#[tauri::command]
fn initial_snapshot(state: tauri::State<'_, AppState>) -> Result<BridgeEnvelope<UiUpdate>, String> {
    tracing::info!(ms = STARTUP.elapsed().as_millis(), "first frame requested");
    Ok(state.bridge()?.initial_snapshot())
}

#[tauri::command]
async fn dispatch(
    state: tauri::State<'_, AppState>,
    action: UiAction,
) -> Result<norte_ui_host::ActionAck, String> {
    state.bridge()?.dispatch(action).await
}

#[tauri::command]
async fn request_snapshot(
    state: tauri::State<'_, AppState>,
) -> Result<norte_ui_host::ActionAck, String> {
    state.bridge()?.request_snapshot().await
}

/// What the renderer measured (task 3.6). Only with the `metrics` feature.
#[cfg(feature = "metrics")]
#[derive(Debug, serde::Deserialize)]
struct MetricSample {
    /// What was measured (`key-to-paint`, `scroll-frame`).
    what: String,
    /// The samples, in milliseconds.
    samples: Vec<f64>,
}

#[cfg(feature = "metrics")]
#[tauri::command]
fn metrics(sample: MetricSample) {
    let mut v = sample.samples.clone();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let pct = |p: f64| -> f64 {
        if v.is_empty() {
            return 0.0;
        }
        let i = (((v.len() - 1) as f64) * p).round() as usize;
        v[i]
    };
    // To stdout: this is a measurement, and whoever is measuring is watching.
    println!(
        "metrics {} n={} p50={:.2}ms p95={:.2}ms max={:.2}ms",
        sample.what,
        v.len(),
        pct(0.50),
        pct(0.95),
        v.last().copied().unwrap_or(0.0)
    );
}

/// The bytes of the open image, RAW.
///
/// `tauri::ipc::Response` and not a serialized `Vec<u8>`: over serde's path, a
/// byte vector crosses as a JSON array of numbers — four or five bytes of
/// text per real byte — which for eight megabytes is absurd.
///
/// An EMPTY vector means "there is no image", which the renderer already
/// knows from the frame: this is not a second source of truth about whether
/// there is an image, only the transport for its bytes.
#[tauri::command]
async fn image_bytes(state: tauri::State<'_, AppState>) -> Result<tauri::ipc::Response, String> {
    let bytes = state.bridge()?.image_bytes().await?;
    Ok(tauri::ipc::Response::new(bytes))
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "`tauri::command` requires `State` by value"
)]
#[tauri::command]
fn catalog(
    state: tauri::State<'_, AppState>,
) -> Result<std::sync::Arc<norte_gui_tauri::catalog::HostCatalog>, String> {
    Ok(state.bridge()?.catalog())
}

/// The window's own title bar (ADR 0136): minimize, maximize, close or drag
/// the window that CALLS.
///
/// Rejects with the native bar: then the desktop already does all of this,
/// and a door nobody needs is not left open.
#[expect(
    clippy::needless_pass_by_value,
    reason = "`tauri::command` requires `State` and the window by value"
)]
#[tauri::command]
fn window_control(
    state: tauri::State<'_, AppState>,
    window: tauri::WebviewWindow,
    verb: norte_gui_tauri::commands::WindowVerb,
) -> Result<(), String> {
    use norte_gui_tauri::commands::WindowVerb;
    if !state.bridge()?.catalog().appearance.custom_titlebar {
        return Err("the title bar is the desktop's".to_owned());
    }
    let done = match verb {
        WindowVerb::Minimize => window.minimize(),
        WindowVerb::ToggleMaximize => match window.is_maximized() {
            Ok(true) => window.unmaximize(),
            Ok(false) => window.maximize(),
            Err(e) => Err(e),
        },
        // `close` goes through `CloseRequested`, so `[ui] confirm_quit` asks
        // just as it does with the desktop's X.
        WindowVerb::Close => window.close(),
        WindowVerb::Drag => window.start_dragging(),
    };
    done.map_err(|e| e.to_string())
}

/// The registered commands. With `metrics`, one more — and only then.
#[cfg(not(feature = "metrics"))]
fn handler() -> impl Fn(tauri::ipc::Invoke) -> bool + Send + Sync + 'static {
    tauri::generate_handler![
        initial_snapshot,
        dispatch,
        request_snapshot,
        catalog,
        image_bytes,
        window_control
    ]
}

#[cfg(feature = "metrics")]
fn handler() -> impl Fn(tauri::ipc::Invoke) -> bool + Send + Sync + 'static {
    tauri::generate_handler![
        initial_snapshot,
        dispatch,
        request_snapshot,
        catalog,
        image_bytes,
        window_control,
        metrics
    ]
}

fn main() -> ExitCode {
    let cli = match startup::parse(std::env::args_os().skip(1)) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("norte-gui: {e}");
            return ExitCode::from(2);
        }
    };
    if cli.help {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    if cli.version {
        println!("norte-gui {}", norte_frontend::version::VERSION_LINE);
        return ExitCode::SUCCESS;
    }
    // Touched here so the reference is the process's startup and not the
    // first time someone reads it.
    let _ = *STARTUP;

    // Startup runs over Tauri's runtime: the tasks the host keeps alive
    // (listing, progress, connection) have to run on the same one.
    let boot_result = tauri::async_runtime::block_on(startup::boot(&cli));
    let state = match boot_result {
        Ok(boot) => {
            let mut cat =
                norte_gui_tauri::catalog::catalog(boot.host.instance(), boot.lang, &boot.theme);
            cat.appearance = boot.appearance;
            cat.first_run = boot.first_run;
            cat.no_splash = boot.no_splash;
            cat.theme_light = boot.theme_light;
            cat.theme_dark = boot.theme_dark;
            AppState::Ready(Box::new(Bridge::new(
                boot.host,
                boot.snapshot,
                cat,
                boot.lang,
            )))
        }
        // Without a daemon there is no screen, but there IS a window: a
        // binary that dies in the terminal tells whoever opened it from a
        // launcher nothing at all.
        Err(e) => {
            tracing::error!(error = %e, "startup failed");
            AppState::Failed(e.to_string())
        }
    };

    let result = tauri::Builder::default()
        .manage(state)
        .invoke_handler(handler())
        .plugin(navigation_guard())
        .setup(|app| {
            // Which binary this is, in the title: version and tree revision.
            // The webview does not need to know it and the title does not go
            // through it.
            if let Some(v) = app.get_webview_window("main") {
                let _ = v.set_title(&format!("norte {}", norte_frontend::version::VERSION_LINE));
            }
            let state: tauri::State<'_, AppState> = app.state();
            // The window's own title bar (ADR 0136): without the desktop's.
            // Here, when it is created, and not in `tauri.conf.json`: there
            // it is fixed, and the stock one has to stay the native one.
            if state
                .bridge()
                .is_ok_and(|b| b.catalog().appearance.custom_titlebar)
                && let Some(v) = app.get_webview_window("main")
            {
                let _ = v.set_decorations(false);
            }
            if let Ok(bridge) = state.bridge() {
                let sub = bridge.host().subscribe();
                let sink = WindowSink {
                    app: app.handle().clone(),
                };
                tauri::async_runtime::spawn(norte_gui_tauri::sink::pump(sub, sink));
                // And the NATIVE effects, over their own channel: clipboard,
                // open with the desktop and terminal. They do not go through
                // the webview — it neither sees the paths nor has permission
                // to run anything — but through this process, with one
                // narrow door per thing (ADR 0066 D11).
                let native_effects = bridge.host().native_effects();
                // The THEME is resolved again in THIS process: colors cross
                // over converted into CSS variables, and that conversion is
                // not the host's. The catalogue is rebuilt and the renderer
                // is told to ask for it again; if the name does not resolve,
                // it is told nothing — repainting for nothing is worse than
                // not repainting.
                let handle = app.handle().clone();
                let apply_theme = move |name: &str| {
                    let state: tauri::State<'_, AppState> = handle.state();
                    if state.bridge().is_ok_and(|b| b.change_theme(name)) {
                        let _ = handle.emit(EVENT_CATALOG, ());
                    }
                };
                // The host travels too, not just the channel: the folder
                // picker ANSWERS it (#284), and that answer comes in through
                // `dispatch` like any other action.
                // And closing: the host asks for it when `[ui] confirm_quit`
                // has nothing left to ask. `CONFIRMED` lets the next
                // `CloseRequested` through without asking again — without
                // that flag the window would never close, which is worse
                // than not asking.
                let close_handle = app.handle().clone();
                let close = move || {
                    CONFIRMED.store(true, std::sync::atomic::Ordering::SeqCst);
                    if let Some(v) = close_handle.get_webview_window("main") {
                        let _ = v.close();
                    }
                };
                tauri::async_runtime::spawn(norte_gui_tauri::nativo::bombear(
                    native_effects,
                    bridge.host_shared(),
                    apply_theme,
                    close,
                ));
            }
            Ok(())
        })
        .on_window_event(on_window_event)
        .run(tauri::generate_context!());

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("norte-gui: {e}");
            ExitCode::FAILURE
        }
    }
}

/// The schemes the webview is allowed to load a page from.
///
/// `tauri:` is the packaged bundle; `ipc:` is how the webview talks to this
/// process. Nothing else.
const PAGE_SCHEMAS: &[&str] = &["tauri", "ipc"];

/// The webview does NOT navigate outside its assets.
///
/// The CSP does not cover TOP-LEVEL navigation — `form-action` is forms and
/// there is no `navigate-to` — so without this, a `window.location =
/// "https://…"` replaces the entire interface with someone else's page
/// INSIDE the application's frame: the window the user believes they are
/// looking at is norte's. Tauri still rejects commands from a remote origin,
/// so what this closes is IMPERSONATION, not IPC (ADR 0066, decision D11).
fn navigation_guard<R: tauri::Runtime>() -> tauri::plugin::TauriPlugin<R> {
    tauri::plugin::Builder::new("norte-navegacion")
        .on_navigation(|_webview, url| {
            let allowed = PAGE_SCHEMAS.contains(&url.scheme());
            if !allowed {
                tracing::warn!(scheme = url.scheme(), "navigation rejected");
            }
            allowed
        })
        .build()
}

/// What to wait for on shutdown before closing the window anyway.
const DEADLINE_OFF: std::time::Duration = std::time::Duration::from_secs(2);

/// The close is already confirmed: the next `CloseRequested` does not ask.
///
/// Set by the host's `CloseWindow` effect, which arrives when the reader
/// answers yes — or when `[ui] confirm_quit` says there is nothing to ask.
/// Without this flag, closing would keep asking in a loop and the window
/// would never close.
static CONFIRMED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The WINDOW events the host needs to know about.
///
/// Outside `main` because they are three unrelated things — focus, what gets
/// dropped and closing — and putting them in the application builder would
/// make them read as part of startup, which is the last thing they are.
fn on_window_event(window: &tauri::Window, event: &tauri::WindowEvent) {
    // Focus, to the host (#285): with the window in front, the desktop does
    // not need to be told, because the bar and the dashboard already show it.
    if let tauri::WindowEvent::Focused(focused) = event {
        let state: tauri::State<'_, AppState> = window.state();
        if let Ok(bridge) = state.bridge() {
            let host = bridge.host_shared();
            let focused = *focused;
            tauri::async_runtime::spawn(async move {
                let _ = host
                    .dispatch(norte_ui_host::UiAction::WindowFocus { focused })
                    .await;
            });
        }
    }
    // What gets DROPPED on the window (#283), and only what gets dropped:
    // `Over`/`Leave` are not forwarded, because the host does not paint
    // drag highlighting and sending them would be traffic for every pixel
    // the pointer crosses. The drop does not copy anything by itself: it
    // opens the confirmation, which is where the reader sees what really
    // arrived.
    if let tauri::WindowEvent::DragDrop(tauri::DragDropEvent::Drop { paths, .. }) = event {
        let state: tauri::State<'_, AppState> = window.state();
        if let Ok(bridge) = state.bridge() {
            let host = bridge.host_shared();
            // As text as-is, without `to_string_lossy`: a name that is not
            // UTF-8 is not converted with replacements, because that would
            // name ANOTHER file. It is dropped here and the host never gets
            // to know about it — the bridge is JSON and there is no way for
            // those bytes to cross it — so the reader sees a shorter list
            // than what they dragged. It is this path's known limit, and the
            // one that sees the whole thing is the pane.
            let paths: Vec<String> = paths
                .iter()
                .filter_map(|p| p.to_str().map(str::to_owned))
                .collect();
            tauri::async_runtime::spawn(async move {
                let _ = host
                    .dispatch(norte_ui_host::UiAction::FilesDropped { paths })
                    .await;
            });
        }
    }
    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
        let state: tauri::State<'_, AppState> = window.state();
        // `[ui] confirm_quit`: asking belongs to the HOST, which is the one
        // that has the configuration and the task dashboard. Closing never
        // used to ask, and `always` is exactly the value the guard calls for.
        //
        // Only the FIRST time: when the reader confirms, the host answers
        // with `CloseWindow`, which sets `CONFIRMED` and closes again.
        // Without that flag the window would never close.
        if !CONFIRMED.swap(false, std::sync::atomic::Ordering::SeqCst)
            && let Ok(bridge) = state.bridge()
        {
            let host = bridge.host_shared();
            let ack = tauri::async_runtime::block_on(async {
                tokio::time::timeout(
                    DEADLINE_OFF,
                    host.dispatch(norte_ui_host::UiAction::RequestQuit),
                )
                .await
            });
            // If the host answered, it decides: it either opened the dialog
            // or asked to close, and in both cases this gesture stops here.
            // If it did NOT answer — stuck socket, dead host — it closes
            // anyway: a window that cannot be closed is worse than one that
            // does not ask.
            if ack.is_ok() {
                api.prevent_close();
                return;
            }
            tracing::warn!("the host did not answer the close question: closing anyway");
        }
        if let Ok(bridge) = state.bridge() {
            // Closing flushes the session: it is the only chance to save
            // where each pane was, and doing it on a loose thread would
            // close it halfway.
            // WITH A DEADLINE: this runs on the event-loop thread and
            // `shutdown` waits for an answer from the daemon. With the
            // socket stuck, the window stopped repainting and never closed
            // — and killing the process is exactly the path that guarantees
            // losing the session.
            let report = tauri::async_runtime::block_on(async {
                tokio::time::timeout(DEADLINE_OFF, bridge.host().shutdown()).await
            });
            match report {
                Ok(Ok(r)) if r.incomplete => {
                    tracing::warn!("work was left unfinished on close");
                }
                Ok(Ok(_)) => {}
                Ok(Err(e)) => tracing::warn!(error = %e, "shutdown failed"),
                Err(_) => tracing::warn!(
                    "shutdown did not answer within {DEADLINE_OFF:?}: the session may \
                         have been left unwritten"
                ),
            }
        }
    }
}
