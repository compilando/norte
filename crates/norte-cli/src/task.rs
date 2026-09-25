//! Ctrl+C handling and the progress loop shared by all the Tasks `norte`
//! runs (`cp`/`mv`/`rm`/`undo`/`sync`/`index`).

use std::process::ExitCode;

use norte_core::backend::TaskRef;
use norte_proto::TaskState;

use crate::EXIT_CANCELLED;

/// Arms a Task's Ctrl+C handler: cancels via its
/// [`norte_core::backend::TaskCanceller`] (hard rule 3) instead of letting
/// the OS kill the process with the default SIGINT.
///
/// Shared between [`run_task`] and `sync.plan` (#180): before this only
/// `run_task` armed it, so a Ctrl+C during `sync_plan_show_apply`'s
/// drain — which does not go through `run_task` — had no handler at all
/// and the process died by SIGINT without running any `Drop`. That
/// matters more here than in `cp`/`mv`/`rm`: a spool's `.part` is only
/// cleaned up if `SpoolWriter::finish`/`Drop` gets to run, and neither
/// one runs when the OS ends the process by signal instead of by a normal
/// return.
///
/// The caller has to `abort()` the returned `JoinHandle` as soon as the
/// Task ends — otherwise the `ctrl_c()` inside stays waiting forever. The
/// watcher for SIGINT of the WHOLE `norte sync`, with a swappable target.
///
/// It exists because a per-phase [`watch_ctrl_c`] is incorrect and this
/// branch proved it (W2 branch review, BLOCKER-1). Registering `ctrl_c()`
/// in tokio is **process-wide and permanent**: tokio's docs say so in so
/// many words — "even if this `Signal` instance is dropped, subsequent
/// `SIGINT` deliveries will end up captured by Tokio, and the default
/// platform behavior will NOT be reset" — so aborting the task that was
/// waiting does NOT give the signal back to the OS.
///
/// With a per-phase watcher, every gap BETWEEN phases is left with a
/// SIGINT tokio swallows and that no longer kills the process. The gap
/// that matters is the `[y/N]` prompt: the spot where a human sits for
/// minutes deciding whether to delete a subtree, and where before #180
/// `Ctrl+C` DID work because nothing had been registered yet.
///
/// A single watcher for the whole invocation, and the phases feed it
/// their canceller. With no canceller set, `Ctrl+C` exits with 130 by
/// itself, which is what the OS used to do. And `take()` instead of
/// reading: the FIRST `Ctrl+C` cancels the task, the SECOND exits — the
/// same pact as the panes' double `Esc`.
pub(crate) struct SigintGate {
    state: std::sync::Arc<std::sync::Mutex<SigintState<norte_core::backend::TaskCanceller>>>,
    _handle: tokio::task::JoinHandle<()>,
}

/// What to do with a `Ctrl+C`, decided ONLY by the watcher's state.
///
/// Separated from the gate so it can be tested without signals or
/// processes: the gap it closes is milliseconds long and cannot be
/// reproduced by hand.
#[derive(Debug, PartialEq, Eq)]
enum SigintAction {
    /// There is a live Task: cancel it (hard rule 3).
    Cancel,
    /// There is a Task BEING BORN: point the `Ctrl+C` at it and cancel it
    /// as soon as it exists. Exiting here would kill the process without
    /// the `Drop` that deletes the spool's `.part` running — which is
    /// exactly what #180 fixed and this window was reopening.
    Defer,
    /// Nothing is alive or being born (the `[y/N]` prompt, the plan on
    /// screen): do what the OS would do.
    Exit,
}

/// The watcher's state. Generic over the canceller so tests do not need a
/// real `TaskRef`.
struct SigintState<C> {
    target: Option<C>,
    /// A Task was requested whose handle has not come back yet.
    arming: bool,
    /// A `Ctrl+C` arrived while it was being born.
    pending: bool,
}

impl<C> Default for SigintState<C> {
    fn default() -> Self {
        Self {
            target: None,
            arming: false,
            pending: false,
        }
    }
}

impl<C> SigintState<C> {
    /// Decides what to do with the signal, taking the canceller if there
    /// is one.
    fn on_signal(&mut self) -> (SigintAction, Option<C>) {
        if let Some(c) = self.target.take() {
            return (SigintAction::Cancel, Some(c));
        }
        if self.arming {
            self.pending = true;
            return (SigintAction::Defer, None);
        }
        (SigintAction::Exit, None)
    }

    /// A Task has been REQUESTED: from here to [`Self::point_at`], a
    /// `Ctrl+C` is parked instead of killing the process.
    fn arming(&mut self) {
        self.arming = true;
    }

    /// The Task now exists. Returns the canceller if it must be used NOW
    /// because the `Ctrl+C` arrived while it was being born.
    fn point_at(&mut self, canceller: C) -> Option<C> {
        self.arming = false;
        if std::mem::take(&mut self.pending) {
            return Some(canceller);
        }
        self.target = Some(canceller);
        None
    }

    /// No live Task anymore: the next `Ctrl+C` exits with 130.
    fn release(&mut self) {
        self.target = None;
        self.arming = false;
        self.pending = false;
    }
}

impl SigintGate {
    /// Arms the watcher. Once per invocation, never per phase.
    pub(crate) fn arm() -> Self {
        let state: std::sync::Arc<
            std::sync::Mutex<SigintState<norte_core::backend::TaskCanceller>>,
        > = std::sync::Arc::default();
        let seen = std::sync::Arc::clone(&state);
        let handle = tokio::spawn(async move {
            loop {
                if tokio::signal::ctrl_c().await.is_err() {
                    break;
                }
                // INVARIANT: the Mutex never gets poisoned — under the
                // lock only Options and bools move, with no possible
                // panic.
                let (action, canceller) = seen.lock().unwrap().on_signal();
                match action {
                    SigintAction::Cancel => {
                        eprintln!("\n{}", norte_i18n::t("cli-cancelling"));
                        if let Some(c) = canceller {
                            c.cancel();
                        }
                    }
                    // The Task has not come back yet: it is parked and
                    // `point_at` cancels it as soon as it exists. Exiting
                    // here would be `exit(130)` with no `Drop`, and the
                    // spool's `.part` would be left orphaned.
                    SigintAction::Defer => eprintln!("\n{}", norte_i18n::t("cli-cancelling")),
                    SigintAction::Exit => {
                        eprintln!();
                        std::process::exit(130);
                    }
                }
            }
        });
        Self {
            state,
            _handle: handle,
        }
    }

    /// A Task has been REQUESTED. Call BEFORE starting it: between the
    /// request and the handle there is a window where a `Ctrl+C` used to
    /// kill the process raw, skipping the `Drop` that deletes the spool's
    /// `.part`.
    pub(crate) fn arming(&self) {
        self.state.lock().unwrap().arming();
    }

    /// This Task is the one a `Ctrl+C` cancels from now on — and if the
    /// signal already arrived while it was being born, it is cancelled
    /// HERE.
    pub(crate) fn point_at(&self, task: &TaskRef) {
        // INVARIANT: as above.
        let already = self.state.lock().unwrap().point_at(task.canceller());
        if let Some(c) = already {
            c.cancel();
        }
    }

    /// No live Task anymore: the next `Ctrl+C` exits with 130.
    pub(crate) fn release(&self) {
        // INVARIANT: as above.
        self.state.lock().unwrap().release();
    }
}

fn watch_ctrl_c(task: &TaskRef) -> tokio::task::JoinHandle<()> {
    let canceller = task.canceller();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            eprintln!("\n{}", norte_i18n::t("cli-cancelling"));
            canceller.cancel();
        }
    })
}

/// The loop shared between [`run_task`] and `sync.apply`: paints progress
/// to stderr, arms [`watch_ctrl_c`], and returns the terminal [`TaskState`]
/// WITHOUT translating it to an exit code or a message.
///
/// The translation lives in each caller on purpose (#187): for
/// `run_task` — `cp`/`mv`/`rm`/`undo` — a `Cancelled` IS "clean
/// destination". For `sync.apply` it is not: what was applied up to the
/// cutoff stays, journaled, and the only frontend that can say how much
/// is the one that asks for `sync.report`. Collapsing both cases into a
/// single translator's `Cancelled` branch is exactly how the CLI ended up
/// being the only one of the three frontends that could not say so.
pub(crate) async fn drive_task(
    task: TaskRef,
    show_bytes: bool,
    sigint: Option<&SigintGate>,
) -> TaskState {
    // With a gate — `norte sync`, which has several phases and a prompt
    // between them — it gets POINTED AT. Without one — `cp`/`mv`/`rm`/`undo`,
    // a single invocation that exits as soon as the Task ends — the usual
    // watcher is enough: the gap `SigintGate` closes does not exist there,
    // because there is nothing afterward.
    let sig = sigint.map_or_else(
        || Some(watch_ctrl_c(&task)),
        |g| {
            g.point_at(&task);
            None
        },
    );

    let mut rx = task.progress();
    loop {
        let snap = rx.borrow_and_update().clone();
        render(&snap, show_bytes);
        if snap.state.is_terminal() {
            break;
        }
        if rx.changed().await.is_err() {
            break;
        }
    }
    match (&sig, sigint) {
        (Some(h), _) => h.abort(),
        (None, Some(g)) => g.release(),
        (None, None) => {}
    }
    let final_state = rx.borrow().state.clone();
    eprintln!();
    final_state
}

/// Runs a Task painting progress to stderr; Ctrl-C cancels cooperatively
/// (the task leaves a clean destination or a `.norte-partial`, hard rule
/// 3).
pub(crate) async fn run_task(task: TaskRef, show_bytes: bool) -> ExitCode {
    match drive_task(task, show_bytes, None).await {
        TaskState::Completed => ExitCode::SUCCESS,
        TaskState::Cancelled => {
            eprintln!("{}", norte_i18n::t("cli-cancelled-clean"));
            ExitCode::from(EXIT_CANCELLED)
        }
        TaskState::Failed { error } => {
            eprintln!(
                "{}",
                norte_i18n::ta("cli-final-error", &[("error", &error.to_string())])
            );
            ExitCode::FAILURE
        }
        other => {
            eprintln!(
                "{}",
                norte_i18n::ta("cli-unexpected-state", &[("state", &format!("{other:?}"))])
            );
            ExitCode::FAILURE
        }
    }
}

fn render(p: &norte_proto::TaskProgress, show_bytes: bool) {
    let entries = match p.entries_total {
        Some(t) => format!("{}/{t}", p.entries_done),
        None => format!("{}/?", p.entries_done),
    };
    if show_bytes {
        let bytes = match p.bytes_total {
            Some(t) if t > 0 => {
                let pct = p.bytes_done.saturating_mul(100) / t;
                format!("{} / {t} bytes ({pct}%)", p.bytes_done)
            }
            _ => format!("{} bytes", p.bytes_done),
        };
        eprint!("\r{bytes} — {entries} entries   ");
    } else {
        eprint!("\r{entries} entries   ");
    }
}

#[cfg(test)]
mod sigint_gate_tests {
    use super::{SigintAction, SigintState};

    /// Regression of the gap `just test` uncovered under load: the Task is
    /// born INSIDE `sync_plan`, and the spool's `.part` with it. A
    /// `Ctrl+C` in that window used to exit via `process::exit(130)` —
    /// with no `Drop`, and therefore with the `.part` orphaned that #180
    /// existed to prevent. The exit code did not distinguish the two
    /// cases: 130 in both.
    #[test]
    fn a_signal_while_the_task_is_being_born_does_not_kill_the_process() {
        let mut state = SigintState::<&str>::default();
        state.arming();
        let (action, canceller) = state.on_signal();
        assert_eq!(action, SigintAction::Defer, "never Exit while being born");
        assert!(canceller.is_none());
        // And as soon as the Task exists, it is cancelled RIGHT AWAY: the
        // signal is not lost.
        assert_eq!(state.point_at("canceller"), Some("canceller"));
    }

    #[test]
    fn with_a_live_task_the_signal_cancels_only_once() {
        let mut state = SigintState::<&str>::default();
        assert_eq!(state.point_at("canceller"), None);
        assert_eq!(state.on_signal(), (SigintAction::Cancel, Some("canceller")));
        // The SECOND Ctrl+C exits, which is the double-Esc pact.
        assert_eq!(state.on_signal(), (SigintAction::Exit, None));
    }

    #[test]
    fn with_nothing_alive_the_signal_exits_like_the_os_would() {
        let mut state = SigintState::<&str>::default();
        assert_eq!(state.on_signal(), (SigintAction::Exit, None));
    }

    /// Releasing the Task also erases a parked `Ctrl+C`: if the one being
    /// born already finished, cancelling the NEXT one would be cancelling
    /// what nobody asked for.
    #[test]
    fn releasing_forgets_the_parked_signal() {
        let mut state = SigintState::<&str>::default();
        state.arming();
        assert_eq!(state.on_signal().0, SigintAction::Defer);
        state.release();
        assert_eq!(
            state.point_at("other"),
            None,
            "the next one is not cancelled"
        );
    }
}
