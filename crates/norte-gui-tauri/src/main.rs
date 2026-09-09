//! El binario del renderer: monta el host, abre UNA ventana y bombea.
#![forbid(unsafe_code)]

use std::process::ExitCode;

use norte_gui_tauri::commands::{AppState, Bridge};
use norte_gui_tauri::sink::{EVENT_CATALOG, EVENT_LAGGED, EVENT_UPDATE, SinkError, UpdateSink};
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
#[expect(
    clippy::needless_pass_by_value,
    reason = "`tauri::command` exige `State` por valor"
)]
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

#[expect(
    clippy::needless_pass_by_value,
    reason = "`tauri::command` exige `State` por valor"
)]
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
        println!("norte-gui {}", norte_frontend::version::VERSION_LINE);
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
            let mut cat =
                norte_gui_tauri::catalog::catalogo(boot.host.instance(), boot.lang, &boot.theme);
            cat.appearance = boot.appearance;
            AppState::Ready(Box::new(Bridge::new(
                boot.host,
                boot.snapshot,
                cat,
                boot.lang,
            )))
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
            // Qué binario es este, en el título: versión y revisión del árbol.
            // La webview no lo necesita saber y el título no pasa por ella.
            if let Some(v) = app.get_webview_window("main") {
                let _ = v.set_title(&format!("norte {}", norte_frontend::version::VERSION_LINE));
            }
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
                // El TEMA vuelve a resolverse en ESTE proceso: los colores
                // cruzan convertidos en variables CSS, y esa conversión no es
                // del host. Se rehace el catálogo y se le dice al renderer que
                // vuelva a pedirlo; si el nombre no resuelve, no se le dice
                // nada — repintar por nada es peor que no repintar.
                let mando = app.handle().clone();
                let aplicar_tema = move |nombre: &str| {
                    let estado: tauri::State<'_, AppState> = mando.state();
                    if estado.bridge().is_ok_and(|b| b.cambiar_tema(nombre)) {
                        let _ = mando.emit(EVENT_CATALOG, ());
                    }
                };
                // El host va también, y no solo el canal: el selector de
                // carpeta le CONTESTA (#284), y esa respuesta entra por
                // `dispatch` como cualquier otra acción.
                // Y cerrar: el host lo pide cuando `[ui] confirm_quit` ya no
                // tiene nada que preguntar. `CONFIRMADO` deja pasar el
                // siguiente `CloseRequested` sin volver a preguntar — sin él
                // la ventana no se cerraría nunca, que es peor que no
                // preguntar.
                let mando_cierre = app.handle().clone();
                let cerrar = move || {
                    CONFIRMADO.store(true, std::sync::atomic::Ordering::SeqCst);
                    if let Some(v) = mando_cierre.get_webview_window("main") {
                        let _ = v.close();
                    }
                };
                tauri::async_runtime::spawn(norte_gui_tauri::nativo::bombear(
                    nativos,
                    bridge.host_compartido(),
                    aplicar_tema,
                    cerrar,
                ));
            }
            Ok(())
        })
        .on_window_event(al_evento_de_ventana)
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

/// El cierre ya está confirmado: el siguiente `CloseRequested` no pregunta.
///
/// Lo pone el efecto `CloseWindow` del host, que es lo que llega cuando el
/// lector contesta que sí —o cuando `[ui] confirm_quit` dice que no hay nada
/// que preguntar—. Sin esta marca, cerrar volvería a preguntar en bucle y la
/// ventana no se cerraría nunca.
static CONFIRMADO: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Los eventos de la VENTANA que el host necesita saber.
///
/// Fuera de `main` porque son tres cosas sin relación entre sí —el foco, lo
/// que se suelta y el cierre— y meterlas en el constructor de la aplicación
/// hace que se lean como parte del arranque, que es lo que menos son.
fn al_evento_de_ventana(window: &tauri::Window, event: &tauri::WindowEvent) {
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
    // Lo que se SUELTA sobre la ventana (#283), y solo lo que se
    // suelta: `Over`/`Leave` no se reenvían, porque el host no pinta
    // realce de arrastre y mandarlos sería tráfico por cada píxel que
    // cruza el puntero. El drop no copia nada por sí solo: abre la
    // confirmación, que es donde el lector ve qué llegó de verdad.
    if let tauri::WindowEvent::DragDrop(tauri::DragDropEvent::Drop { paths, .. }) = event {
        let estado: tauri::State<'_, AppState> = window.state();
        if let Ok(bridge) = estado.bridge() {
            let host = bridge.host_compartido();
            // A texto tal cual, sin `to_string_lossy`: un nombre que
            // no sea UTF-8 no se convierte con reemplazos, porque eso
            // nombraría OTRO fichero. Se descarta aquí y el host no
            // llega a saberlo —el puente es JSON y no hay forma de que
            // esos bytes lo crucen—, así que el lector ve una lista
            // más corta que lo que arrastró. Es el límite conocido de
            // esta vía, y el que la ve entera es el panel.
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
        let estado: tauri::State<'_, AppState> = window.state();
        // `[ui] confirm_quit`: preguntar es del HOST, que es quien tiene la
        // configuración y el tablero de tasks. Cerrar no preguntaba nunca, y
        // `always` es justo el valor que pide la guarda.
        //
        // Solo la PRIMERA vez: cuando el lector confirma, el host contesta
        // con `CloseWindow`, que marca `CONFIRMADO` y vuelve a cerrar. Sin esa
        // marca la ventana no se cerraría nunca.
        if !CONFIRMADO.swap(false, std::sync::atomic::Ordering::SeqCst)
            && let Ok(bridge) = estado.bridge()
        {
            let host = bridge.host_compartido();
            let ack = tauri::async_runtime::block_on(async {
                tokio::time::timeout(
                    PLAZO_APAGADO,
                    host.dispatch(norte_ui_host::UiAction::RequestQuit),
                )
                .await
            });
            // Si el host contestó, él decide: o abrió el diálogo o pidió
            // cerrar, y en los dos casos este gesto se detiene aquí. Si NO
            // contestó —socket atascado, host muerto— se cierra igual: una
            // ventana que no se puede cerrar es peor que una que no pregunta.
            if ack.is_ok() {
                api.prevent_close();
                return;
            }
            tracing::warn!("el host no contestó a la pregunta de cerrar: se cierra igual");
        }
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
}
