//! `spawn_blocking` que conserva el span (ADR 0127).
//!
//! `tokio::task::spawn_blocking` NO hereda el span actual: lo que se registra
//! dentro del cierre sale sin padre, sin el `task_id` ni el `rpc` que lo
//! lanzaron, y es justo ahí donde está la E/S. Este ayudante captura el span
//! en el hilo que llama y lo entra en el hilo bloqueante. `clippy.toml` del
//! crate prohíbe la llamada directa para que el agujero no vuelva.

/// Como [`tokio::task::spawn_blocking`], pero el cierre corre dentro del span
/// que estaba activo al llamar.
#[allow(
    clippy::disallowed_methods,
    reason = "el único sitio que puede llamarla: aquí se le añade el span"
)]
pub(crate) fn spawn_blocking<F, R>(f: F) -> tokio::task::JoinHandle<R>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    let span = tracing::Span::current();
    tokio::task::spawn_blocking(move || span.in_scope(f))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use tracing::Subscriber;
    use tracing_subscriber::Layer;
    use tracing_subscriber::layer::{Context, SubscriberExt as _};
    use tracing_subscriber::registry::LookupSpan;

    /// Por cada evento, los nombres de sus spans de fuera a dentro.
    #[derive(Clone, Default)]
    struct Padres(Arc<Mutex<Vec<Vec<String>>>>);

    impl<S: Subscriber + for<'a> LookupSpan<'a>> Layer<S> for Padres {
        fn on_event(&self, event: &tracing::Event<'_>, ctx: Context<'_, S>) {
            let cadena = ctx
                .event_scope(event)
                .map(|scope| scope.from_root().map(|s| s.name().to_owned()).collect())
                .unwrap_or_default();
            self.0.lock().expect("padres").push(cadena);
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn un_evento_del_hilo_bloqueante_cuelga_de_su_tarea() {
        let padres = Padres::default();
        let subscriber = tracing_subscriber::registry().with(padres.clone());
        let dispatch = tracing::Dispatch::new(subscriber);

        // El hilo bloqueante no hereda el subscriber por defecto del hilo
        // del test; `with_default` alrededor del cierre sí se lo da, y lo que
        // queda por probar es exactamente el span.
        let d = dispatch.clone();
        let _guard = tracing::dispatcher::set_default(&dispatch);
        let span = tracing::info_span!("task", task_id = 7);
        let fut = {
            let _e = span.enter();
            super::spawn_blocking(move || {
                tracing::dispatcher::with_default(&d, || tracing::info!("dentro"));
            })
        };
        fut.await.expect("el cierre vuelve");

        let vistos = padres.0.lock().expect("padres").clone();
        assert_eq!(vistos, vec![vec!["task".to_owned()]], "{vistos:?}");
    }
}
