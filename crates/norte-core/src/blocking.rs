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

/// Como [`tokio::spawn`], pero el future corre dentro del span que estaba
/// activo al lanzarlo (ADR 0127).
///
/// Sin esto, lo que registra una tarea lanzada desde una petición sale sin su
/// `rpc`. Cuando no hay span activo adjunta el vacío, que no cambia nada.
/// Lo que NO debe heredar el span de quien la lanza va por [`spawn_raiz`].
/// Un test de fuente (`tests/spans_en_spawn.rs`) impide llamar a
/// `tokio::spawn` a pelo fuera de este módulo.
pub(crate) fn spawn<F>(fut: F) -> tokio::task::JoinHandle<F::Output>
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    use tracing::Instrument as _;
    tokio::spawn(fut.instrument(tracing::Span::current()))
}

/// Como [`tokio::spawn`], SIN el span de quien la lanza: la tarea es la raíz
/// de lo suyo, a propósito.
///
/// Dos sitios, y los dos tienen su porqué escrito donde se llama: una
/// CONEXIÓN del daemon (cada `rpc` es raíz, ADR 0127; heredando, todas las
/// peticiones de meses colgarían de `run`, el span de la vida entera del
/// daemon) y el CORREDOR del scheduler (no corre necesariamente el job que
/// lo lanzó, y cada job trae su span). Un nombre distinto para que la
/// decisión se vea en el sitio, y no una llamada a pelo que parezca un
/// olvido.
pub(crate) fn spawn_raiz<F>(fut: F) -> tokio::task::JoinHandle<F::Output>
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    tokio::spawn(fut)
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

    /// Lo mismo para una tarea `async` lanzada con [`super::spawn`]: su evento
    /// cuelga del span que estaba activo al lanzarla.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn un_evento_de_una_tarea_lanzada_cuelga_de_su_tarea() {
        let padres = Padres::default();
        let dispatch = tracing::Dispatch::new(tracing_subscriber::registry().with(padres.clone()));
        let _guard = tracing::dispatcher::set_default(&dispatch);
        let span = tracing::info_span!("task", task_id = 8);
        let d = dispatch.clone();
        let h = {
            let _e = span.enter();
            super::spawn(async move {
                tracing::dispatcher::with_default(&d, || tracing::info!("dentro"));
            })
        };
        h.await.expect("la tarea vuelve");
        let vistos = padres.0.lock().expect("padres").clone();
        assert_eq!(vistos, vec![vec!["task".to_owned()]], "{vistos:?}");
    }
}
