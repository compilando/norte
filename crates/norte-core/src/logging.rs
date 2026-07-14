//! Inicialización de tracing para los binarios (cli/daemon).
//!
//! Cap de SEGURIDAD (issue #43, regla 10): `suppaftp` loguea cada comando del
//! canal de control a nivel TRACE del crate `log`, incluido `PASS <password>`.
//! El bridge `tracing-log` (feature default de `tracing-subscriber`) lo
//! materializaría con `RUST_LOG=trace`. [`init`] añade una directiva estática
//! `suppaftp=info` AL FINAL del filtro, así que gana a cualquier `RUST_LOG`
//! —incluido `suppaftp=trace` explícito— y la password nunca llega al sink.

use tracing_subscriber::EnvFilter;
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

/// Instala el subscriber global (fmt a stderr) con el cap de seguridad.
/// Idempotente y no-fatal: si ya hay un subscriber, no hace nada.
pub fn init() {
    let _ = tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .with(filter_from(None))
        .try_init();
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
}
