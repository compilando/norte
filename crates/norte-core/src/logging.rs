//! Inicialización de tracing para los binarios (cli/daemon).
//!
//! Cap de SEGURIDAD (issue #43, regla 10): `suppaftp` loguea cada comando del
//! canal de control a nivel TRACE del crate `log`, incluido `PASS <password>`.
//! El bridge `tracing-log` (feature default de `tracing-subscriber`) lo
//! materializaría con `RUST_LOG=trace`. [`init`] añade una directiva estática
//! `suppaftp=info` AL FINAL del filtro, así que gana a cualquier `RUST_LOG`
//! —incluido `suppaftp=trace` explícito— y la password nunca llega al sink.

use std::path::Path;

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::Layer;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::prelude::*;

/// Construye el `EnvFilter`: default INFO, respeta `RUST_LOG` (o `env` si se
/// pasa, para tests), y SIEMPRE capa `suppaftp` a `info` como última directiva
/// (regla 10, no configurable).
fn filter_from(env: Option<&str>) -> EnvFilter {
    let base = match env {
        Some(s) => EnvFilter::builder()
            .with_default_directive(LevelFilter::INFO.into())
            .parse_lossy(s),
        None => EnvFilter::builder()
            .with_default_directive(LevelFilter::INFO.into())
            .from_env_lossy(),
    };
    // Última directiva de igual especificidad gana → cap duro.
    base.add_directive("suppaftp=info".parse().expect("directiva estática válida"))
}

/// Prefijo de los ficheros rotados. La rotación es DIARIA, así que el nombre
/// real lleva la fecha detrás.
const LOG_PREFIX: &str = "norte.log";

/// La capa de fichero y su guard, o `None` si el directorio no se pudo crear.
///
/// **El guard hay que SOSTENERLO mientras dure el proceso.** El writer es no
/// bloqueante y su hilo vacía la cola al soltarlo: un `let _ = …` aquí
/// perdería las últimas líneas, que son justamente las del fallo que se está
/// diagnosticando.
///
/// `None` en vez de `Err`: un log es diagnóstico, y un diagnóstico que impide
/// arrancar es peor que no tenerlo.
fn file_layer<S>(dir: &Path, retain: usize) -> Option<(impl Layer<S>, WorkerGuard)>
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    std::fs::create_dir_all(dir).ok()?;
    let appender = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix(LOG_PREFIX)
        .max_log_files(retain)
        .build(dir)
        .ok()?;
    let (writer, guard) = tracing_appender::non_blocking(appender);
    let layer = tracing_subscriber::fmt::layer()
        // Un fichero no es una terminal: los códigos de color solo lo ensucian.
        .with_ansi(false)
        .with_writer(writer);
    Some((layer, guard))
}

/// Cuántos ficheros rotados se conservan cuando la config no dice otra cosa.
/// Una semana: suficiente para que un fallo de ayer siga estando, poco para
/// que esto crezca sin que nadie lo mire.
const RETAIN_DEFAULT: usize = 7;

/// Dónde va el log: `[log] dir`, o `<state_dir>/logs`.
///
/// `None` = no hay directorio de estado (una CI pelada, un servicio sin `HOME`)
/// y por tanto no hay fichero. El caller degrada.
#[must_use]
pub fn log_dir(configured: Option<&Path>) -> Option<std::path::PathBuf> {
    match configured {
        Some(d) => Some(d.to_path_buf()),
        None => norte_config::dirs::state_dir().map(|s| s.join("logs")),
    }
}

/// Instala el subscriber global: stderr MÁS el fichero rotatorio, con el cap de
/// seguridad. Para `norte-cli` y el daemon.
///
/// Idempotente y no-fatal: si ya hay un subscriber, no hace nada. Devuelve el
/// guard del writer de fichero (ver [`file_layer`]) — **hay que sostenerlo**—,
/// o `None` si no hubo fichero que abrir.
#[must_use]
pub fn init() -> Option<WorkerGuard> {
    init_with(true, None)
}

/// Como [`init`] pero SOLO al fichero. Para los frontends.
///
/// La TUI no instalaba subscriber ninguno, y lo decía en un comentario: un
/// `fmt` a stderr pelea con la pantalla alternativa, así que cada
/// `tracing::warn!` que saliera de ahí se descartaba mudo. La GUI tampoco
/// instalaba ninguno. Los dos frontends que un usuario ejecuta de verdad no
/// producían un solo diagnóstico; esto es lo que lo arregla, y sin escribir un
/// byte en una pantalla que están dibujando.
#[must_use]
pub fn init_to_file(configured: Option<&Path>) -> Option<WorkerGuard> {
    init_with(false, configured)
}

/// El montaje común. `stderr` decide si va también la capa de terminal.
fn init_with(stderr: bool, configured: Option<&Path>) -> Option<WorkerGuard> {
    let (file, guard) = match log_dir(configured) {
        Some(d) => match file_layer(&d, RETAIN_DEFAULT) {
            Some((l, g)) => (Some(l), Some(g)),
            None => (None, None),
        },
        None => (None, None),
    };
    let terminal = stderr.then(|| tracing_subscriber::fmt::layer().with_writer(std::io::stderr));
    let _ = tracing_subscriber::registry()
        .with(file)
        .with(terminal)
        .with(filter_from(None))
        .try_init();
    guard
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tracing::Level;
    use tracing_subscriber::Layer;
    use tracing_subscriber::layer::Context;

    /// Capa que registra (target, nivel) de cada evento que la ATRAVIESA (ya
    /// filtrado): lo que aquí llega es exactamente lo que el sink loguearía.
    struct Collect(Arc<Mutex<Vec<(String, Level)>>>);
    impl<S: tracing::Subscriber> Layer<S> for Collect {
        fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
            let m = event.metadata();
            self.0
                .lock()
                .expect("lock")
                .push((m.target().to_string(), *m.level()));
        }
    }

    #[test]
    fn suppaftp_trace_capped_even_with_trace_env() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::registry()
            .with(Collect(Arc::clone(&seen)))
            .with(filter_from(Some("trace,suppaftp=trace")));

        tracing::subscriber::with_default(subscriber, || {
            tracing::trace!(target: "suppaftp", "PASS hunter2"); // debe CAERSE
            tracing::info!(target: "suppaftp", "conectado"); // pasa
            tracing::trace!(target: "otro", "visible"); // pasa (no capado)
        });

        let seen = seen.lock().expect("lock");
        // La password (evento TRACE de suppaftp) NUNCA atraviesa el filtro.
        assert!(
            !seen.contains(&("suppaftp".to_string(), Level::TRACE)),
            "suppaftp TRACE debe estar capado: {seen:?}"
        );
        // Pero info de suppaftp y trace de otros targets sí.
        assert!(seen.contains(&("suppaftp".to_string(), Level::INFO)));
        assert!(seen.contains(&("otro".to_string(), Level::TRACE)));
    }

    /// Los ficheros que hay en `dir`, con su contenido concatenado.
    fn volcado(dir: &std::path::Path) -> (usize, String) {
        let ficheros: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
            .expect("listar")
            .map(|e| e.expect("entrada").path())
            .collect();
        let texto = ficheros
            .iter()
            .map(|f| std::fs::read_to_string(f).unwrap_or_default())
            .collect();
        (ficheros.len(), texto)
    }

    /// El appender escribe de verdad, en el directorio que se le da.
    #[test]
    fn el_log_aterriza_en_un_fichero() {
        let dir = tempfile::tempdir().expect("tmp");
        let (layer, guard) = file_layer(dir.path(), 3).expect("appender");
        let sub = tracing_subscriber::registry()
            .with(layer)
            .with(filter_from(None));
        tracing::subscriber::with_default(sub, || {
            tracing::info!(target: "norte_core::prueba", "una linea");
        });
        drop(guard);

        let (cuantos, texto) = volcado(dir.path());
        assert_eq!(cuantos, 1, "un fichero de log");
        assert!(texto.contains("una linea"), "el evento está: {texto}");
    }

    /// **El cap de `suppaftp` cubre el FICHERO igual que cubre stderr.**
    ///
    /// Se filtra en el registry, antes de cualquier capa, así que debería
    /// seguirse de la arquitectura — y por eso mismo se comprueba: «debería
    /// seguirse» no es una prueba, y lo que está en juego es una contraseña en
    /// un fichero que PERSISTE, que es peor que una que pasó por una terminal
    /// (regla dura 10).
    #[test]
    fn la_password_de_ftp_no_llega_al_fichero() {
        let dir = tempfile::tempdir().expect("tmp");
        let (layer, guard) = file_layer(dir.path(), 3).expect("appender");
        let sub = tracing_subscriber::registry()
            .with(layer)
            .with(filter_from(Some("trace,suppaftp=trace")));
        tracing::subscriber::with_default(sub, || {
            tracing::trace!(target: "suppaftp", "PASS hunter2");
            tracing::info!(target: "suppaftp", "conectado");
        });
        drop(guard);

        let (_, texto) = volcado(dir.path());
        assert!(
            !texto.contains("hunter2"),
            "la password llegó al fichero: {texto}"
        );
        assert!(texto.contains("conectado"), "y lo que sí pasa, pasa");
    }

    /// Un directorio que no se puede crear NO tumba el programa: se degrada.
    /// Un log es diagnóstico, y un diagnóstico que impide arrancar es peor que
    /// no tenerlo.
    #[test]
    fn un_directorio_imposible_degrada_en_vez_de_fallar() {
        let dir = tempfile::tempdir().expect("tmp");
        // Un FICHERO donde debería ir el directorio: `create_dir_all` falla.
        let ocupado = dir.path().join("ocupado");
        std::fs::write(&ocupado, b"no soy un directorio").expect("fichero");
        let capa = file_layer::<tracing_subscriber::Registry>(&ocupado, 3);
        assert!(capa.is_none(), "no se puede crear ahí, así que no hay capa");
    }
}
