//! Shutting the host down.
//!
//! Part of `controller`: these are methods of `Estado`, moved here without
//! touching them (ADR 0086). The only writer is still the actor.

// These modules are the same `impl Estado` split into pieces, so they use
// the same imports as the parent. Listing them here would be a forty-line
// list per file, across 32 files, that goes out of sync the moment the
// parent imports something — `super::*` keeps it in sync on its own.
#[allow(clippy::wildcard_imports)]
use super::*;

impl Estado {
    /// Shuts down: flushes the session if this window owns it, and SAYS if
    /// something was left unwritten.
    ///
    /// Flushing here and not only on a tick is what makes closing right
    /// after navigating save the new directory and not the previous one. A
    /// last-moment conflict is not retried wildly: it is reported, which is
    /// the only honest thing to do once there is no screen left to fix it.
    pub(super) async fn apagar(&mut self, backend: &dyn HostBackend) -> ShutdownReport {
        // A task still alive at shutdown is unfinished work, whether the
        // session says so or not: closing mid-copy and reporting "all good"
        // is exactly what this report exists not to do.
        let has_tasks = self.tasks.values().any(|t| {
            matches!(
                t.vista.state,
                crate::dto::TaskStateView::Queued
                    | crate::dto::TaskStateView::Running
                    | crate::dto::TaskStateView::Paused
            )
        });
        if !self.sesion.owner || self.sesion.futuro {
            // A loose window does not write, and a future session is not
            // clobbered.
            return ShutdownReport {
                incomplete: has_tasks,
            };
        }
        let now = u64::try_from(ahora_ms()).unwrap_or(0);
        let mut body = self.capturar_sesion();
        // With a tick's write IN FLIGHT, another is not sent on top of it: it
        // would carry the same revision and one of the two would conflict for
        // sure. If what was being written is what's there now, there is
        // nothing left to write; if not, the last one did not make it, and
        // that is what gets reported.
        if let Some(in_flight) = &self.sesion.en_vuelo {
            return ShutdownReport {
                incomplete: has_tasks || **in_flight != body,
            };
        }
        let alive: Vec<SlotId> = self.huecos.keys().map(|id| SlotId(*id)).collect();
        if self.sesion.policy.prepare(&mut body, &alive, now).is_none() {
            // Nothing changed since the last thing sent.
            return ShutdownReport {
                incomplete: has_tasks,
            };
        }
        let Ok(json) = serde_json::to_value(&body) else {
            return ShutdownReport { incomplete: true };
        };
        let put_result = backend
            .session_put(
                norte_frontend::session::SCHEMA_VERSION,
                self.sesion.revision,
                json,
            )
            .await;
        // The body going OVER size is not the same as a conflict, and
        // treating it the same was losing the reader's whole screen without
        // saying anything (#316): the core refuses the FULL `put` and keeps
        // whatever was stored, i.e. where the reader was days ago. The TUI
        // already degraded; this window did not, which is the silent
        // divergence from ADR 0077.
        //
        // The same thing the TUI drops is dropped here, because the decision
        // is shared (`SessionBody::degrade_for_size`), and it is retried
        // ONCE: if it does not fit even without history, there is nothing
        // left to degrade other than where the reader is, and that is what
        // needed saving.
        // `degrade_for_size` goes in the `if`'s BODY and not in a `match`
        // guard: it mutates `body`, and a guard with a side effect is a trap
        // for whoever touches it next.
        let mut put_result = put_result;
        if matches!(put_result, Err(Error::LimitExceeded { .. })) && body.degrade_for_size() {
            put_result = match serde_json::to_value(&body) {
                Ok(json) => {
                    backend
                        .session_put(
                            norte_frontend::session::SCHEMA_VERSION,
                            self.sesion.revision,
                            json,
                        )
                        .await
                }
                // Same as twelve lines up: a body that does not serialize is
                // "not written", not a core panic.
                Err(_) => return ShutdownReport { incomplete: true },
            };
        }
        // A conflict is another window that wrote in between: theirs stays,
        // and ours is reported as not having made it. Overwriting it would
        // lose someone's session.
        let Ok(rev) = put_result else {
            self.sesion.policy.resend();
            return ShutdownReport { incomplete: true };
        };
        self.sesion.revision = rev;
        self.sesion.policy.sent(std::sync::Arc::new(body));
        ShutdownReport {
            incomplete: has_tasks,
        }
    }
}
