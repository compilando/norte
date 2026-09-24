//! [`Backend`](super::Backend)'s event-channel area: foreign tasks,
//! connection events, policy approvals, connection degradation/failure,
//! plugin notices (`hook`) and lazy-journal warnings.

use std::sync::Arc;

use tokio::sync::mpsc;

use super::{
    Backend, ChannelConnectionObserver, ChannelFailureObserver, ChannelHookSink,
    ChannelJournalSink, ConnEvent, TaskRef,
};

impl Backend {
    /// Channel for FOREIGN tasks (queued by other frontends of the same
    /// session). `None` when embedded or if already taken. Only the
    /// connection's original owner should call it; a clone (scripting)
    /// should not.
    pub fn take_foreign_tasks(&mut self) -> Option<mpsc::UnboundedReceiver<TaskRef>> {
        match self {
            Self::Embedded(_) => None,
            #[cfg(unix)]
            Self::Remote(r) => {
                // The SDK delivers REMOTE tasks; a frontend talks in terms
                // of `TaskRef` and doesn't want to know where it came from.
                // The bridge is a forwarding task because a channel can't
                // be mapped in place: it dies when the source channel dies,
                // so it doesn't outlive the connection that fed it.
                let mut source = r.take_foreign_tasks()?;
                let (tx, rx) = mpsc::unbounded_channel();
                crate::blocking::spawn(async move {
                    while let Some(t) = source.recv().await {
                        if tx.send(TaskRef::from(t)).is_err() {
                            break;
                        }
                    }
                });
                Some(rx)
            }
        }
    }

    /// Channel for connection events (reconnection notices). `None` when
    /// embedded or if already taken. Only the connection's original owner
    /// should call it; a clone (scripting) should not.
    pub fn take_conn_events(&mut self) -> Option<mpsc::UnboundedReceiver<ConnEvent>> {
        match self {
            Self::Embedded(_) => None,
            #[cfg(unix)]
            Self::Remote(r) => r.take_conn_events(),
        }
    }

    /// Channel for pending policy approvals (M3-3b T5): every daemon
    /// `policy.approval_required` (and the `policy.pending` resync on
    /// (re)connect) arrives here for the frontend to ask the human. `None`
    /// when embedded (no agents to approve over this path) or if already
    /// taken. Only the connection's original owner should call it; a clone
    /// (scripting) should not.
    pub fn take_approvals(
        &mut self,
    ) -> Option<mpsc::UnboundedReceiver<norte_proto::methods::PolicyApprovalRequired>> {
        match self {
            Self::Embedded(_) => None,
            #[cfg(unix)]
            Self::Remote(r) => r.take_approvals(),
        }
    }

    /// Receiver for `connection.degraded` notices (#44). On `Remote` it
    /// comes from the daemon's pump; on `Embedded` it INSTALLS an observer
    /// on the engine that pushes to a channel — so BOTH modes surface
    /// degradation uniformly (rust MAJOR M1 + security m1: embedded used to
    /// be silent). One-shot by nature (installs/takes once); on `Embedded`
    /// the notice is SYNCHRONOUS (the observer fires inside the current
    /// command's `provider_for`), so a later drain sees it with no race.
    pub fn take_degraded(
        &mut self,
    ) -> Option<mpsc::UnboundedReceiver<norte_proto::methods::ConnectionDegraded>> {
        match self {
            Self::Embedded(engine) => {
                let (tx, rx) = mpsc::unbounded_channel();
                // Chaining: the slot belongs to ONE, and this `take_*` must
                // not silence whatever else already set one (#322).
                engine.chain_connection_observer(|previo| {
                    Arc::new(ChannelConnectionObserver { tx, previo })
                });
                Some(rx)
            }
            #[cfg(unix)]
            Self::Remote(r) => r.take_degraded(),
        }
    }

    /// Receiver for `connection.failed` failures (#322): WHY a connection
    /// did NOT open. Twin of [`Backend::take_degraded`] and treated the same
    /// on both arms — on `Remote` it comes from the daemon's pump, on
    /// `Embedded` it installs an observer.
    ///
    /// Exists on `Embedded` and not only on `Remote` because the diagnosis
    /// used to get lost on BOTH: on the daemon it stayed in its log, and on
    /// embedded it went out through the process's own stderr — which the
    /// TUI's alternate screen swallows. A failure that gets diagnosed or
    /// not depending on the transport is the worst way for it to depend on
    /// anything.
    ///
    /// One-shot on `Remote`, where the first owner takes the receiver. On
    /// `Embedded` it is NOT —same as [`Backend::take_degraded`]—: every call
    /// chains another observer and returns another receiver, and one nobody
    /// drains is a channel with no ceiling that only grows. Call it ONCE, at
    /// startup.
    ///
    /// By `&self` and not `&mut self` like [`Backend::take_degraded`]:
    /// neither branch needed it, and `norte connect` —the command typed
    /// precisely to diagnose this— holds the backend by shared reference.
    /// Requiring `&mut` would have shut out the one place where the human
    /// is explicitly asking "why won't it connect?".
    pub fn take_failed(
        &self,
    ) -> Option<mpsc::UnboundedReceiver<norte_proto::methods::ConnectionFailed>> {
        match self {
            Self::Embedded(engine) => {
                let (tx, rx) = mpsc::unbounded_channel();
                engine.chain_connection_observer(|previo| {
                    Arc::new(ChannelFailureObserver { tx, previo })
                });
                Some(rx)
            }
            #[cfg(unix)]
            Self::Remote(r) => r.take_failed(),
        }
    }

    /// Receiver for `plugin.notice` notices (0.69.0, ADR 0100): a `hook`
    /// plugin's sentence about a mutation already recorded, or that a
    /// plugin's hooks turned themselves off after three failures. On
    /// `Remote` it comes from the daemon's pump; on `Embedded` it STARTS the
    /// hook dispatcher over this engine's journal and gives it a channel —
    /// so both modes run the same hooks and surface the same things.
    /// One-shot, like [`Backend::take_failed`]: the engine has ONE slot for
    /// the dispatcher and whoever asks for it second gets `None`, instead of
    /// starting another one that would clobber the first.
    ///
    /// Also `None` on `Embedded` if the engine carries no journal (no rows,
    /// no hooks) or if the WASM runtime could not be created: with no
    /// runtime no plugin runs, and neither does a hook (fail-closed, with a
    /// trace). The dispatcher only dies once the returned receiver is
    /// dropped.
    ///
    /// # Panics
    /// Outside a tokio runtime: it starts tasks.
    pub fn take_plugin_notices(
        &self,
    ) -> Option<mpsc::UnboundedReceiver<norte_proto::methods::PluginNotice>> {
        match self {
            Self::Embedded(engine) => {
                if !engine.has_journal() || !engine.claim_hooks_slot() {
                    return None;
                }
                let runtime = match norte_plugin_host::PluginRuntime::new() {
                    Ok(r) => Arc::new(r),
                    Err(e) => {
                        tracing::warn!(error = %e, "hooks: no plugin runtime, none will run");
                        return None;
                    }
                };
                let (tx, rx) = mpsc::unbounded_channel();
                // Plugging in the journal is `async` (the lazy one keeps the
                // end under its lock) and reading `policy.toml` is I/O
                // (rule 2): both in one task. A mutation that gets ahead of
                // this ends up with no hook, and this is the startup: there
                // isn't one yet. No cancellation token of its own: the
                // embedded dispatcher's life is the receiver's
                // (`is_closed`), and the process hosting it ends along with it.
                let engine = Arc::clone(engine);
                crate::blocking::spawn(async move {
                    // The human's rules apply here too (ADR 0101): the
                    // embedded engine carries no gate, so the dispatcher
                    // checks them for the `plugin` actor. An unreadable file
                    // is reported and counts as none.
                    let policy = crate::blocking::spawn_blocking(crate::PolicyConfig::load)
                        .await
                        .ok()
                        .and_then(|r| match r {
                            Ok(p) => Some(Arc::new(p)),
                            Err(e) => {
                                tracing::warn!(error = %e, "hooks: policy.toml unreadable, no rules");
                                None
                            }
                        });
                    let (sender, _task) = crate::hooks::spawn_dispatcher(
                        crate::connect::config_dir(),
                        runtime,
                        Arc::new(ChannelHookSink { tx }),
                        tokio_util::sync::CancellationToken::new(),
                        Some(crate::hooks::SidecarWriter {
                            engine: Arc::downgrade(&engine),
                            scopes: None,
                            policy,
                        }),
                    );
                    engine.enable_hooks(sender).await;
                });
                Some(rx)
            }
            #[cfg(unix)]
            Self::Remote(r) => r.take_plugin_notices(),
        }
    }

    /// Receiver for the "this session is NOT being recorded in the journal"
    /// notice (#167/#177). Only `Embedded` can end up with no journal —the
    /// daemon refuses to start without one—, so on `Remote` this is `None`.
    ///
    /// Like [`Backend::take_degraded`], on `Embedded` it INSTALLS the sink
    /// on the engine instead of taking an already-built channel. Call it at
    /// startup, before the first mutation; and if a mutation gets ahead of
    /// it anyway, the notice isn't lost (the `LazyJournal` holds onto it
    /// until there's a sink).
    ///
    /// BOTH LOSSES AND RECOVERIES arrive (#179): the ownership window can
    /// reopen, so a frontend that only listens for
    /// [`JournalStatus::Lost`](crate::embedded::JournalStatus::Lost) ends up
    /// painting "this session isn't recorded" over one that is.
    ///
    /// Also `None` if the embedded engine carries no lazy journal —one built
    /// with `Engine::new()`, which journals NOTHING and will never warn
    /// about it—: returning a channel there would be telling the frontend
    /// it's covered by a notice that can never arrive.
    ///
    /// ONCE only, like its siblings: a second sink silences the first
    /// receiver (see [`crate::embedded::LazyJournal::set_warning_sink`]).
    pub fn take_journal_warnings(
        &mut self,
    ) -> Option<mpsc::UnboundedReceiver<crate::embedded::JournalStatus>> {
        match self {
            Self::Embedded(engine) => {
                let (tx, rx) = mpsc::unbounded_channel();
                engine
                    .set_journal_warning_sink(Arc::new(ChannelJournalSink { tx }))
                    .then_some(rx)
            }
            #[cfg(unix)]
            Self::Remote(_) => None,
        }
    }
}
