//! El binario del renderer: monta el host, abre UNA ventana y bombea.

use std::process::ExitCode;

use norte_gui_tauri::commands::{AppState, Bridge};
use norte_gui_tauri::sink::{EVENT_LAGGED, EVENT_UPDATE, SinkError, UpdateSink};
use norte_gui_tauri::startup::{self, USAGE};
use norte_ui_host::{BridgeEnvelope, UiAction, dto::UiUpdate};
use tauri::{Emitter, Manager};

/// Cuándo arrancó el proceso. Es la referencia del arranque en frío: lo que
/// se mide es de aquí a que el renderer pide su primera foto, que es cuando
/// la webview existe, ha cargado su script y ya puede pintar.
static ARRANQUE: std::sync::LazyLock<std::time::Instant> =
    std::sync::LazyLock::new(std::time::Instant::now);

/// El sumidero de verdad: la ventana principal.
struct VentanaSink {
    app: tauri::AppHandle,
}

impl UpdateSink for VentanaSink {
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

// `tauri::State` va POR VALOR en un comando: es lo que el macro genera, y no
// hay una versión por referencia. El lint no conoce esa restricción.
#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
fn initial_snapshot(state: tauri::State<'_, AppState>) -> Result<BridgeEnvelope<UiUpdate>, String> {
    tracing::info!(ms = ARRANQUE.elapsed().as_millis(), "primera foto pedida");
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

/// Lo que el renderer midió (tarea 3.6). Solo con la feature `metrics`.
#[cfg(feature = "metrics")]
#[derive(Debug, serde::Deserialize)]
struct MetricSample {
    /// Qué se midió (`key-to-paint`, `scroll-frame`).
    what: String,
    /// Las muestras, en milisegundos.
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
    // A stdout: esto es una medida, y quien mide la está mirando.
    println!(
        "metrics {} n={} p50={:.2}ms p95={:.2}ms max={:.2}ms",
        sample.what,
        v.len(),
        pct(0.50),
        pct(0.95),
        v.last().copied().unwrap_or(0.0)
    );
}

/// Los bytes de la imagen abierta, CRUDOS.
///
/// `tauri::ipc::Response` y no un `Vec<u8>` serializado: por el camino de
/// serde, un vector de bytes cruza como un array JSON de números —cuatro o
/// cinco bytes de texto por byte real—, que para ocho megas es absurdo.
///
/// Un vector VACÍO significa «no hay imagen», que es lo que el renderer ya
/// sabe por la foto: esto no es una segunda fuente de verdad sobre si hay
/// imagen, solo el transporte de sus bytes.
#[tauri::command]
async fn image_bytes(state: tauri::State<'_, AppState>) -> Result<tauri::ipc::Response, String> {
    let bytes = state.bridge()?.image_bytes().await?;
    Ok(tauri::ipc::Response::new(bytes))
}

#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
fn catalog(
    state: tauri::State<'_, AppState>,
) -> Result<std::sync::Arc<norte_gui_tauri::catalog::HostCatalog>, String> {
    Ok(state.bridge()?.catalog())
}

/// Los comandos registrados. Con `metrics`, uno más — y solo entonces.
#[cfg(not(feature = "metrics"))]
fn handler() -> impl Fn(tauri::ipc::Invoke) -> bool + Send + Sync + 'static {
    tauri::generate_handler![
        initial_snapshot,
        dispatch,
        request_snapshot,
        catalog,
        image_bytes
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
        println!("norte-gui {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    // Se toca aquí para que la referencia sea el arranque del proceso y no la
    // primera vez que alguien la lee.
    let _ = *ARRANQUE;

    // El arranque va sobre el runtime de Tauri: las tasks que el host deja
    // vivas (listado, progreso, conexión) tienen que correr en el mismo.
    let arranque = tauri::async_runtime::block_on(startup::boot(&cli));
    let state = match arranque {
        Ok(boot) => {
            let cat =
                norte_gui_tauri::catalog::catalogo(boot.host.instance(), boot.lang, &boot.theme);
            AppState::Ready(Box::new(Bridge::new(boot.host, boot.snapshot, cat)))
        }
        // Sin daemon no hay pantalla, pero SÍ hay ventana: un binario que
        // muere en el terminal no le dice nada a quien lo abrió desde un
        // lanzador.
        Err(e) => {
            tracing::error!(error = %e, "el arranque falló");
            AppState::Failed(e.to_string())
        }
    };

    let resultado = tauri::Builder::default()
        .manage(state)
        .invoke_handler(handler())
        .plugin(guardia_de_navegacion())
        .setup(|app| {
            let estado: tauri::State<'_, AppState> = app.state();
            if let Ok(bridge) = estado.bridge() {
                let sub = bridge.host().subscribe();
                let sink = VentanaSink {
                    app: app.handle().clone(),
                };
                tauri::async_runtime::spawn(norte_gui_tauri::sink::pump(sub, sink));
                // Y los efectos NATIVOS, por su propio canal: portapapeles,
                // abrir con el escritorio y terminal. No pasan por la
                // webview —no ve las rutas ni tiene permiso para ejecutar
                // nada— sino por este proceso, con una puerta estrecha por
                // cosa (ADR 0066 D11).
                let nativos = bridge.host().native_effects();
                // El host va también, y no solo el canal: el selector de
                // carpeta le CONTESTA (#284), y esa respuesta entra por
                // `dispatch` como cualquier otra acción.
                tauri::async_runtime::spawn(norte_gui_tauri::nativo::bombear(
                    nativos,
                    bridge.host_compartido(),
                ));
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            // El foco, al host (#285): con la ventana delante no se avisa por
            // el escritorio, porque la barra y el tablero ya lo cuentan.
            if let tauri::WindowEvent::Focused(focused) = event {
                let estado: tauri::State<'_, AppState> = window.state();
                if let Ok(bridge) = estado.bridge() {
                    let host = bridge.host_compartido();
                    let focused = *focused;
                    tauri::async_runtime::spawn(async move {
                        let _ = host
                            .dispatch(norte_ui_host::UiAction::WindowFocus { focused })
                            .await;
                    });
                }
            }
            if matches!(event, tauri::WindowEvent::CloseRequested { .. }) {
                let estado: tauri::State<'_, AppState> = window.state();
                if let Ok(bridge) = estado.bridge() {
                    // Cerrar vuelca la sesión: es la única oportunidad de
                    // guardar dónde estaba cada panel, y hacerlo en un hilo
                    // suelto sería cerrarla a medias.
                    // CON PLAZO: esto corre en el hilo del bucle de eventos y
                    // `apagar` espera una respuesta del daemon. Con el socket
                    // atascado, la ventana dejaba de repintarse y no se
                    // cerraba nunca — y matar el proceso es justo el camino
                    // que garantiza perder la sesión.
                    let informe = tauri::async_runtime::block_on(async {
                        tokio::time::timeout(PLAZO_APAGADO, bridge.host().shutdown()).await
                    });
                    match informe {
                        Ok(Ok(r)) if r.incomplete => {
                            tracing::warn!("quedó trabajo sin terminar al cerrar");
                        }
                        Ok(Ok(_)) => {}
                        Ok(Err(e)) => tracing::warn!(error = %e, "el apagado falló"),
                        Err(_) => tracing::warn!(
                            "el apagado no contestó en {PLAZO_APAGADO:?}: la sesión puede \
                             haberse quedado sin escribir"
                        ),
                    }
                }
            }
        })
        .run(tauri::generate_context!());

    match resultado {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("norte-gui: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Los esquemas desde los que la webview puede cargar una página.
///
/// `tauri:` es el bundle empaquetado; `ipc:` es cómo la webview habla con
/// este proceso. Nada más.
const ESQUEMAS_DE_PAGINA: &[&str] = &["tauri", "ipc"];

/// La webview NO navega fuera de sus assets.
///
/// La CSP no cubre la navegación de PRIMER NIVEL —`form-action` son
/// formularios y no existe `navigate-to`—, así que sin esto un
/// `window.location = "https://…"` sustituye la interfaz entera por una
/// página ajena DENTRO del marco de la aplicación: la ventana que el usuario
/// cree estar mirando es la de norte. Tauri sigue rechazando los comandos
/// desde un origen remoto, así que lo que esto cierra es la SUPLANTACIÓN, no
/// el IPC (ADR 0066, decisión D11).
fn guardia_de_navegacion<R: tauri::Runtime>() -> tauri::plugin::TauriPlugin<R> {
    tauri::plugin::Builder::new("norte-navegacion")
        .on_navigation(|_webview, url| {
            let permitido = ESQUEMAS_DE_PAGINA.contains(&url.scheme());
            if !permitido {
                tracing::warn!(scheme = url.scheme(), "navegación rechazada");
            }
            permitido
        })
        .build()
}

/// Lo que se espera al apagar antes de cerrar la ventana de todas formas.
const PLAZO_APAGADO: std::time::Duration = std::time::Duration::from_secs(2);
