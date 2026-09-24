//! Execution of a command: a !Send future that the main loop polls inline
//! (NEVER `tokio::spawn`). Cancellation on two fronts (rule 3):
//! 1. the engine Tasks launched by the run (registered cancellers);
//! 2. the script itself, via the instruction hook (kills pure Lua loops).
//!
//! A script stuck in C (`os.execute`) responds to neither: a hard timeout
//! and the driver ABANDONS the future (drop); the Lua state is thrown away
//! on the next reload. Abandoning with a remote submit in flight no longer
//! leaves an orphan Task: dropping the binding fires the backend's
//! `rpc.cancel` guard (#74, pattern #72) and the daemon's dispatch dies
//! pre-effect.

use std::cell::{Cell, RefCell};
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll};
use std::time::Duration;

use mlua::{HookTriggers, Lua, VmState};
use norte_core::backend::Backend;
use tokio_util::sync::CancellationToken;

use super::api::LuaHost;
use super::fs::{self, PaneCtx, RunCancellers};

/// Default hard timeout for a run: a DOCUMENTED contract of
/// [`LuaHost::invoke`] (that is why it is `pub`, re-exported in `lua`);
/// configurable in v2 (see spec).
pub const DEFAULT_TIMEOUT: Duration = Duration::from_mins(5);

/// Grace period after requesting cancellation: margin for the instruction
/// hook to kill the coroutine; once exhausted, the driver abandons just like
/// with the timeout.
const CANCEL_GRACE: Duration = Duration::from_secs(5);

/// Cadence of the run's instruction hook. Lua hooks are PER
/// THREAD (coroutine): it is installed on the command's coroutine BEFORE
/// starting it — `Lua::set_hook` after the fact does not work, since it
/// targets the main state and would never fire inside the run. Every
/// `HOOK_EVERY` instructions the hook returns [`VmState::Yield`]: the
/// coroutine yields control to the executor (poll → `Pending` + immediate
/// wake in mlua), which is what lets the token/deadline arms of the
/// `select!` get a chance to run — without it, a pure Lua loop would
/// monopolize the poll forever and neither cancellation nor timeout could
/// ever fire.
const HOOK_EVERY: u32 = 4096;

/// Outcome of a Lua command run.
#[derive(Debug)]
pub enum RunOutcome {
    /// The command finished; `norte.ui.message` messages accumulated.
    Ok {
        /// The run's messages, in order (capped at `MESSAGES_MAX`, see
        /// `fs.rs`).
        messages: Vec<String>,
    },
    /// Lua error (RAW diagnostic detail: the caller sanitizes and localizes
    /// it before painting it, pattern #73 — it never goes to the bar as is).
    Err {
        /// `Display` of the mlua error, unsanitized.
        detail: String,
        /// Messages accumulated up to the error.
        messages: Vec<String>,
    },
    /// Cancelled by the user (token). Any Lua error following the
    /// cancellation request counts as `Cancelled` (the hook kills the
    /// script with an artificial error).
    Cancelled,
    /// Hard timeout: the driver abandoned the run's future.
    TimedOut,
}

/// Future of a command run (!Send: mlua lives on the main task). Obtained
/// from [`LuaHost::invoke`] and polled inline by the main loop.
///
/// Carries `run_active` as a VALUE (not only inside the future it wraps):
/// `CommandRun` exists from the instant `invoke_with_timeout` returns it,
/// whether it ever gets polled or not. Its [`Drop`] is the ONLY source of
/// truth that DECREMENTS `run_active` — see there for why (spec-review 2,
/// task 7): a guard built INSIDE the future (like `RunGuard`, further
/// below) would NEVER run if the caller creates the `CommandRun` and drops
/// it without ever polling it (an `async fn`'s body does not execute
/// anything until the first poll) — `run_active` would get stuck and the
/// bar frozen until the next hot-reload. With the counter on the type
/// itself, that path is structurally impossible: it does not depend on the
/// caller's (T8) discipline of polling to completion.
///
/// **Drop the value as soon as you have the [`RunOutcome`]** (spec-review
/// 3): while an already-resolved `CommandRun` stays alive somewhere (e.g. an
/// `Option<CommandRun>` slot that has not yet been set to `None`),
/// `LuaHost::statusbar` still counts it as "in flight" and the bar stays
/// frozen for longer than it should.
///
/// `run_active` is a COUNTER (`Rc<Cell<u32>>`), not a boolean: the caller's
/// natural pattern `self.run = Some(host.invoke(...))` evaluates the RHS
/// (which INCREMENTS for the new run) before dropping the old value that
/// occupied the slot (which DECREMENTS) — with a boolean, that decrement of
/// the old one would clobber the `true` the new one had just turned on,
/// leaving it unprotected. With a counter, the two cancel out.
pub struct CommandRun {
    future: Pin<Box<dyn Future<Output = RunOutcome>>>,
    run_active: Rc<Cell<u32>>,
}

impl Future for CommandRun {
    type Output = RunOutcome;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<RunOutcome> {
        self.future.as_mut().poll(cx)
    }
}

impl Drop for CommandRun {
    fn drop(&mut self) {
        // Covers all THREE paths: polled to a terminal outcome, abandoned
        // halfway (dropping a `Pending` future), or never polled at all
        // (immediate drop after `invoke_with_timeout`, with no await
        // whatsoever — `run_command`'s body never got to execute, so no
        // internal `RunGuard` ran). Saturating: the counter should never
        // reach 0 and try to go lower (each `CommandRun` decrements at most
        // once, in ITS OWN Drop), but `saturating_sub` is cheap insurance
        // against a future counting bug — underflowing a `u32` in release
        // would be worse (wraps to `u32::MAX`, the bar would NEVER
        // unfreeze).
        self.run_active.set(self.run_active.get().saturating_sub(1));
    }
}

/// The run's RAII: no matter what happens (normal return, cancellation, or
/// the future being ABANDONED — the `Drop` also runs when the future is
/// dropped halfway):
/// - `remove_hook`: the run's hook lives on its coroutine (which dies with
///   the future), but mlua stores the closure in a slot SHARED PER Lua
///   state; clearing it keeps it from surviving the run (the Lua state is
///   SHARED between runs and with the statusbar);
/// - closes the run (`closed = true`): the stashed fs bindings die;
/// - cancels the registered cancellers (double-cancel is harmless: an
///   already-cancelled token or a terminal task are no-ops; this covers
///   timeout abandonment, where nothing else would cancel them).
///
/// Does NOT touch `run_active` (task 7, spec-review 2): that responsibility
/// now lives in [`CommandRun`]'s `Drop` — this guard runs INSIDE the
/// future, so a `CommandRun` that is never polled would leave it never
/// executed (see `CommandRun`'s rustdoc).
struct RunGuard {
    lua: Lua,
    closed: Rc<Cell<bool>>,
    cancellers: RunCancellers,
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        self.lua.remove_hook();
        self.closed.set(true);
        // Defensive: a panic in Drop is an abort. Today no borrow of
        // `cancellers` crosses an await (invariant of fs.rs), but if a
        // future change broke that and the future were abandoned with the
        // borrow held, this Drop must NOT bring down the process — better
        // to skip the double-cancel (best-effort) than to abort.
        if let Ok(cs) = self.cancellers.try_borrow() {
            for c in cs.iter() {
                c.cancel();
            }
        }
    }
}

/// A run's shared channels/flags, grouped into a single value so as not to
/// overflow `run_command`'s argument count (each one is a cheap
/// `Rc`/`Rc<RefCell<_>>` to move).
struct RunChannels {
    cancellers: RunCancellers,
    messages: Rc<RefCell<Vec<String>>>,
    closed: Rc<Cell<bool>>,
}

impl LuaHost {
    /// Starts command `name` with the default timeout. See
    /// [`LuaHost::invoke_with_timeout`].
    #[must_use]
    pub fn invoke(
        &self,
        name: &str,
        backend: Backend,
        ctx: PaneCtx,
        token: CancellationToken,
    ) -> Option<CommandRun> {
        self.invoke_with_timeout(name, backend, ctx, token, DEFAULT_TIMEOUT)
    }

    /// Starts command `name`: installs FRESH bindings (`install_fs`, a
    /// snapshot of `ctx` and new run channels) and returns the run's
    /// future, or `None` if the command does not exist.
    ///
    /// The caller is responsible for SERIALIZATION: one run at a time per
    /// host (there is one Lua state; two concurrent runs would clobber
    /// bindings and hook). The FIFO queue lives in the TUI's run loop (task
    /// 8), not here.
    ///
    /// Cancellation (rule 3): cancelling `token` cancels the engine Tasks
    /// registered by the run AND the coroutine's instruction hook (installed
    /// from the start, see `HOOK_EVERY`) starts erroring — killing pure Lua
    /// loops; if within `CANCEL_GRACE` the script has not died (stuck in C,
    /// e.g. `os.execute`), or if `timeout` expires, the driver ABANDONS the
    /// future (drop) — the internal guard cleans up the hook and closes the
    /// run either way.
    #[must_use]
    pub fn invoke_with_timeout(
        &self,
        name: &str,
        backend: Backend,
        ctx: PaneCtx,
        token: CancellationToken,
        timeout: Duration,
    ) -> Option<CommandRun> {
        // Registry borrow RELEASED before touching Lua (invariant).
        let f = self.command_fn(name)?;
        let lua = self.lua_handle();
        let run_active = self.run_active_handle();
        let cancellers: RunCancellers = Rc::default();
        let messages: Rc<RefCell<Vec<String>>> = Rc::default();
        let closed: Rc<Cell<bool>> = Rc::default();

        if let Err(e) = fs::install_fs(
            &lua,
            backend,
            ctx,
            Rc::clone(&cancellers),
            Rc::clone(&messages),
            Rc::clone(&closed),
        ) {
            // Half-done installation: the run is closed (no partial binding
            // survives) and the run resolves immediately to Err — never a
            // panic. There is NO hook at all involved in this path, but
            // `run_active` is incremented ANYWAY, for symmetry with
            // `CommandRun`'s Drop's unconditional decrement: every
            // `CommandRun` that leaves here decrements exactly once when it
            // dies, so each one must increment exactly once when it is
            // born — making it conditional would break that counting
            // invariant.
            closed.set(true);
            let detail = e.to_string();
            run_active.set(run_active.get().saturating_add(1));
            return Some(CommandRun {
                future: Box::pin(async move {
                    RunOutcome::Err {
                        detail,
                        messages: Vec::new(),
                    }
                }),
                run_active,
            });
        }

        // The run starts HERE: it is incremented BEFORE returning the
        // value, and its decrement lives in `CommandRun`'s `Drop` (not in an
        // internal future guard) — so it does not even matter whether the
        // caller polls the returned value or drops it immediately (see
        // `CommandRun`'s rustdoc). A counter, not a boolean (spec-review 3):
        // the pattern `self.run = Some(host.invoke(...))` increments for
        // the new run BEFORE the old run's (which occupied the slot) `Drop`
        // decrements — with a boolean, that decrement would clobber the
        // `true` just set.
        run_active.set(run_active.get().saturating_add(1));

        let channels = RunChannels {
            cancellers,
            messages,
            closed,
        };
        Some(CommandRun {
            future: Box::pin(run_command(lua, f, token, timeout, channels)),
            run_active,
        })
    }
}

/// The run's body: a pinned call + a `select!` loop (a selected-out
/// `call_async` cannot be "resumed"; the loop keeps the SAME call future
/// across the cancellation request).
///
/// The command runs on its own coroutine (`Thread`) with its instruction
/// hook installed BEFORE starting it (hooks are per thread, see
/// `HOOK_EVERY`): under normal operation the hook YIELDS control to the
/// executor; after the cancellation request (shared `cancel_flag`) it
/// switches to ERRORING and the coroutine dies within at most `HOOK_EVERY`
/// Lua instructions.
async fn run_command(
    lua: Lua,
    f: mlua::Function,
    token: CancellationToken,
    timeout: Duration,
    channels: RunChannels,
) -> RunOutcome {
    let RunChannels {
        cancellers,
        messages,
        closed,
    } = channels;
    // The guard lives INSIDE the future: if the driver abandons it (drop in
    // the grace/deadline arms… or the caller drops the CommandRun after
    // polling it at least once), the Drop still runs and the shared Lua
    // state is left clean. `run_active` does NOT live here (see the
    // rustdoc of `CommandRun`/`RunGuard`) — this guard would never run if
    // the `CommandRun` is dropped without ever being polled.
    let _guard = RunGuard {
        lua: lua.clone(),
        closed,
        cancellers: Rc::clone(&cancellers),
    };

    let take_messages = || std::mem::take(&mut *messages.borrow_mut());

    let thread = match lua.create_thread(f) {
        Ok(t) => t,
        Err(e) => {
            return RunOutcome::Err {
                detail: e.to_string(),
                messages: take_messages(),
            };
        }
    };
    // Cancellation front 2, armed from the start: the flag is turned on by
    // the token's arm; the hook sees it on the next batch of instructions.
    // While it is off, the periodic yield returns control to the select
    // (essential: without it a `while true do end` would block this poll
    // forever).
    let cancel_flag = Rc::new(Cell::new(false));
    {
        let cancel_flag = Rc::clone(&cancel_flag);
        thread.set_hook(
            HookTriggers::new().every_nth_instruction(HOOK_EVERY),
            move |_, _| {
                if cancel_flag.get() {
                    Err(mlua::Error::RuntimeError("cancelled".into()))
                } else {
                    Ok(VmState::Yield)
                }
            },
        );
    }

    let mut call = std::pin::pin!(thread.into_async::<()>(()));
    let mut cancel_requested = false;
    let mut deadline = std::pin::pin!(tokio::time::sleep(timeout));
    // Only polled after the cancellation request (guarded by the flag);
    // when it fires it is reset to "now + grace".
    let mut grace = std::pin::pin!(tokio::time::sleep(CANCEL_GRACE));

    loop {
        tokio::select! {
            biased;
            r = &mut call => {
                break match r {
                    Ok(()) => RunOutcome::Ok { messages: take_messages() },
                    // Error following the cancellation request: it IS the
                    // cancellation (the hook kills it with "cancelled", but
                    // ANY error after cancellation counts as one).
                    Err(_) if cancel_requested => RunOutcome::Cancelled,
                    Err(e) => RunOutcome::Err {
                        detail: e.to_string(),
                        messages: take_messages(),
                    },
                };
            }
            () = token.cancelled(), if !cancel_requested => {
                cancel_requested = true;
                // Front 1: the engine Tasks launched by the run.
                for c in cancellers.borrow().iter() {
                    c.cancel();
                }
                // Front 2: the coroutine's hook switches to erroring.
                cancel_flag.set(true);
                grace
                    .as_mut()
                    .reset(tokio::time::Instant::now() + CANCEL_GRACE);
            }
            // Grace exhausted (script stuck in C, immune to the hook):
            // ABANDONS the in-flight call (dropped on exit; a remote submit
            // in flight withdraws itself — the backend's rpc.cancel guard,
            // #74).
            () = &mut grace, if cancel_requested => break RunOutcome::Cancelled,
            // Hard timeout: ABANDONS just the same (#74 ditto). If the user
            // had already cancelled (cancel at t≈timeout, with the grace
            // period still running), the honest outcome is Cancelled, not
            // TimedOut — whoever cancelled should not see "time ran out".
            // The race is hard to force in a test without a script stuck in
            // C (the hook kills pure Lua loops in microseconds): only the
            // fix matters.
            () = &mut deadline => {
                break if cancel_requested {
                    RunOutcome::Cancelled
                } else {
                    RunOutcome::TimedOut
                };
            }
        }
    }
    // The guard cleans up (remove_hook + closed + cancel) on this path too.
}
