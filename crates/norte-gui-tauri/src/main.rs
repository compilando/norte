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
    tauri::generate_handler![initial_snapshot, dispatch, request_snapshot, catalog]
}

#[cfg(feature = "metrics")]
fn handler() -> impl Fn(tauri::ipc::Invoke) -> bool + Send + Sync + 'static {
    tauri::generate_handler![
        initial_snapshot,
        dispatch,
        request_snapshot,
        catalog,
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
    logging();
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
            }
            Ok(())
        })
        .on_window_event(|window, event| {
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

/// Ficheros de log que sobreviven a la rotación.
const RETENCION: usize = 7;

/// El log va al FICHERO y solo al fichero.
///
/// Montado aquí y no con el del core a propósito: este binario habla con el
/// daemon por un socket y no debe arrastrar el motor, los providers y el host
/// de plugins para escribir una línea de log (ADR 0066). Es duplicación de
/// MONTAJE, no de reglas; cuando haya un segundo consumidor, se iza.
fn logging() {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    let Some(dir) = norte_config::dirs::state_dir().map(|d| d.join("logs")) else {
        return;
    };
    // 0700 en el directorio y 0600 en cada fichero: aquí dentro va por dónde
    // ha navegado el usuario. El helper del core lo endurece así, y duplicar
    // el MONTAJE (ADR 0066) no era licencia para dejarse la protección.
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
        if !dir.exists()
            && std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&dir)
                .is_err()
        {
            return;
        }
        if let Ok(md) = std::fs::metadata(&dir) {
            let mut perms = md.permissions();
            perms.set_mode(0o700);
            let _ = std::fs::set_permissions(&dir, perms);
        }
    }
    #[cfg(not(unix))]
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let appender = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("norte-gui.log")
        // Retención acotada: un log que crece para siempre es un log que
        // nadie borra.
        .max_log_files(RETENCION)
        .build(&dir);
    let Ok(appender) = appender else {
        return;
    };
    // NO bloqueante: escribir el log es I/O, y este proceso lo hace desde
    // dentro del runtime (regla 2). El guard se filtra a propósito — vive lo
    // que el proceso, y soltarlo dejaría de escribir.
    let (writer, guard) = tracing_appender::non_blocking(appender);
    std::mem::forget(guard);
    let _ = tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(writer)
                .with_ansi(false),
        )
        .with(tracing_subscriber::EnvFilter::new(
            std::env::var("NORTE_LOG").unwrap_or_else(|_| "warn".to_owned()),
        ))
        .try_init();
}
