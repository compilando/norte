//! `spawn_blocking` that preserves the span (ADR 0127).
//!
//! `tokio::task::spawn_blocking` does NOT inherit the current span: whatever
//! gets logged inside the closure comes out with no parent, without the
//! `task_id` or the `rpc` that launched it — and that is exactly where the
//! I/O is. This helper captures the span on the calling thread and enters it
//! on the blocking thread. The crate's `clippy.toml` forbids the direct call
//! so the hole doesn't come back.

/// Like [`tokio::task::spawn_blocking`], but the closure runs inside the span
/// that was active when it was called.
#[allow(
    clippy::disallowed_methods,
    reason = "the one place allowed to call it: the span is added right here"
)]
pub(crate) fn spawn_blocking<F, R>(f: F) -> tokio::task::JoinHandle<R>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    let span = tracing::Span::current();
    tokio::task::spawn_blocking(move || span.in_scope(f))
}

/// Like [`tokio::spawn`], but the future runs inside the span that was
/// active when it was launched (ADR 0127).
///
/// Without this, whatever a task launched from a request logs comes out
/// without its `rpc`. When there is no active span it attaches the empty
/// one, which changes nothing. What must NOT inherit the span of whoever
/// launches it goes through [`spawn_raiz`]. A source test
/// (`tests/spans_en_spawn.rs`) blocks calling bare `tokio::spawn` outside
/// this module.
pub(crate) fn spawn<F>(fut: F) -> tokio::task::JoinHandle<F::Output>
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    use tracing::Instrument as _;
    tokio::spawn(fut.instrument(tracing::Span::current()))
}

/// Like [`tokio::spawn`], WITHOUT the span of whoever launches it: the task
/// is the root of its own, on purpose.
///
/// Two call sites, and both have their reason written where they call it: a
/// daemon CONNECTION (each `rpc` is a root, ADR 0127; if it inherited, months
/// of requests would hang off `run`, the span for the daemon's entire life)
/// and the scheduler's RUNNER (it does not necessarily run the job that
/// launched it, and each job brings its own span). A different name so the
/// decision shows at the call site, instead of a bare call that looks like
/// an oversight.
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

    /// For each event, the names of its spans from outside in.
    #[derive(Clone, Default)]
    struct Parents(Arc<Mutex<Vec<Vec<String>>>>);

    impl<S: Subscriber + for<'a> LookupSpan<'a>> Layer<S> for Parents {
        fn on_event(&self, event: &tracing::Event<'_>, ctx: Context<'_, S>) {
            let chain = ctx
                .event_scope(event)
                .map(|scope| scope.from_root().map(|s| s.name().to_owned()).collect())
                .unwrap_or_default();
            self.0.lock().expect("parents").push(chain);
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn an_event_from_the_blocking_thread_hangs_off_its_task() {
        let parents = Parents::default();
        let subscriber = tracing_subscriber::registry().with(parents.clone());
        let dispatch = tracing::Dispatch::new(subscriber);

        // The blocking thread does not inherit the test thread's default
        // subscriber; `with_default` around the closure does give it one,
        // and what's left to prove is exactly the span.
        let d = dispatch.clone();
        let _guard = tracing::dispatcher::set_default(&dispatch);
        let span = tracing::info_span!("task", task_id = 7);
        let fut = {
            let _e = span.enter();
            super::spawn_blocking(move || {
                tracing::dispatcher::with_default(&d, || tracing::info!("inside"));
            })
        };
        fut.await.expect("the closure returns");

        let seen = parents.0.lock().expect("parents").clone();
        assert_eq!(seen, vec![vec!["task".to_owned()]], "{seen:?}");
    }

    /// The same for an `async` task launched with [`super::spawn`]: its
    /// event hangs off the span that was active when it was launched.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn an_event_from_a_launched_task_hangs_off_its_task() {
        let parents = Parents::default();
        let dispatch = tracing::Dispatch::new(tracing_subscriber::registry().with(parents.clone()));
        let _guard = tracing::dispatcher::set_default(&dispatch);
        let span = tracing::info_span!("task", task_id = 8);
        let d = dispatch.clone();
        let h = {
            let _e = span.enter();
            super::spawn(async move {
                tracing::dispatcher::with_default(&d, || tracing::info!("inside"));
            })
        };
        h.await.expect("the task returns");
        let seen = parents.0.lock().expect("parents").clone();
        assert_eq!(seen, vec![vec!["task".to_owned()]], "{seen:?}");
    }
}
