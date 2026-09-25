//! Statusbar hook (M4 Lua, task 7): `norte.ui.statusbar(fn)` lets an
//! `init.lua` paint the status bar. Unlike a command (task 5), this call is
//! SYNCHRONOUS and fires on EVERY render pass — an infinite loop would
//! block the entire TUI — so the instruction budget does NOT yield (like a
//! command's does): once exhausted it ABORTS the call (`Err`) and
//! `LuaHost::statusbar` disables the hook for the rest of this host's life
//! (it is only re-enabled with a new `LuaHost`, hot-reload, task 8).
//!
//! Context inherited from T5, corrected after review: `Lua::set_hook`
//! installs on the MAIN state; a SYNCHRONOUS `Function::call` runs on that
//! same state (unlike `call_async`, which runs on its own coroutine) —
//! that is why the main state's hook DOES fire here.
//!
//! **CORRECTION (spec-reviewer, reproduced outside the repo with mlua
//! 0.10.5):** the original claim that the driver's hook ("per thread") and
//! this bar's hook ("main state") are independent is FALSE. mlua stores
//! `hook_callback`/`hook_thread` in a single `ExtraData` slot SHARED by the
//! main state and ALL coroutines — mlua's own docs say so: "cannot have
//! more than one hook function set at a time". The C trampoline
//! (`hook_proc`, `state/raw.rs::set_thread_hook`) checks
//! `hook_thread == state` on every firing; if it does not match, it
//! self-disarms (`lua_sethook(state, None, 0, 0)`) WITHOUT calling the
//! callback. If this bar calls `Lua::set_hook`/`remove_hook` while the
//! driver has a live coroutine with its own `Thread::set_hook`
//! (cancellation, rule 3), the next time the driver's hook fires it finds
//! `hook_thread` pointing at ANOTHER state and shuts itself off — a pure
//! Lua loop in flight becomes UNCANCELLABLE (neither token nor timeout
//! kills it; the driver would have to wait out the hard timeout and
//! ABANDON the future).
//!
//! That is why `LuaHost` carries `run_active: Rc<Cell<u32>>` (shared with
//! `driver.rs`: `invoke_with_timeout` INCREMENTS it on start, `CommandRun`'s
//! `Drop` — not `RunGuard`, which lives INSIDE the future and would never
//! run if the `CommandRun` is dropped without polling — DECREMENTS it, on
//! ALL paths including abandonment). It is a COUNTER, not a boolean
//! (spec-review 3): the caller's (T8) natural pattern `self.run =
//! Some(host.invoke(...))` evaluates the new run (increments) BEFORE
//! dropping the old one (decrements) — with a boolean, that old decrement
//! would turn off the protection of the new one that just started. While
//! the counter is `!= 0`, [`super::LuaHost::statusbar`] NEVER calls this
//! function — it does not even remotely touch `set_hook`/`remove_hook` —
//! it returns the cached value if the `StatusInput` matches, or `None`
//! otherwise: the bar FREEZES while any run is alive (including one
//! already finished that the caller has not yet dropped), in exchange for
//! never being able to disarm an in-flight run's cancellation.

use mlua::{Function, HookTriggers, Lua};

/// Instruction budget for one call to the statusbar hook. It fires on
/// EVERY render pass (unlike a command, which runs once): a low cap keeps
/// a slow script from being perceived as a freeze before it gets disabled.
const STATUSBAR_BUDGET: u32 = 50_000;

/// Snapshot of the state visible to the hook. `PartialEq` is the key of
/// [`super::LuaHost::statusbar`]'s cache: two equal snapshots do not
/// re-invoke the script.
#[derive(Debug, Clone, PartialEq)]
pub struct StatusInput {
    /// Wire bytes of the focused pane's cwd.
    pub cwd: Vec<u8>,
    /// Index of the entry under the cursor.
    pub selected: usize,
    /// Total bytes of the marked selection.
    pub selected_bytes: u64,
    /// Number of entries in the current listing.
    pub entries: usize,
    /// Tasks in flight.
    pub tasks: usize,
}

/// RAII: `remove_hook` ALWAYS on exit (normal return or an early `?`). The
/// hook lives in a Lua state slot SHARED with the rest of the host
/// (commands included, `driver.rs`); leaving it set after an error or a
/// panic-catch would contaminate any later synchronous call on the same
/// state. `pub(super)`: also reused by `eval_layer`'s budget (`api.rs`) —
/// one single piece for the pattern.
pub(super) struct HookGuard<'a>(pub(super) &'a Lua);

impl Drop for HookGuard<'_> {
    fn drop(&mut self) {
        self.0.remove_hook();
    }
}

/// Invokes `f` with the `input` snapshot under an instruction budget.
/// SYNCHRONOUS on purpose: the statusbar hook is a short blocking call on
/// the render thread, not a background command (that is `driver.rs`).
///
/// The return value is the script's RAW string, unsanitized — the caller
/// ([`super::LuaHost::statusbar`]) passes it through
/// `crate::app::detail_for_bar` (masks bidi/control + length cap) before
/// showing it.
///
/// # Errors
/// Exhausted budget, a script runtime error (including the script not
/// returning something coercible to a string), or any attempt to yield
/// control (e.g. calling an async API from this synchronous context
/// produces an mlua error) — all indistinguishable to the caller: any of
/// them disables the hook.
///
/// # Caller invariant
/// [`super::LuaHost::statusbar`] NEVER calls this function while
/// `run_active` is on (a Lua command in flight): mlua's hook slot is
/// UNIQUE per instance — shared between the main state and ALL
/// coroutines — and `set_hook`/`remove_hook` here would silently disarm
/// the driver's cancellation hook (see the module docs).
pub(super) fn call_hook(lua: &Lua, f: &Function, input: &StatusInput) -> mlua::Result<String> {
    lua.set_hook(
        HookTriggers::new().every_nth_instruction(STATUSBAR_BUDGET),
        |_, _| {
            Err(mlua::Error::RuntimeError(
                "statusbar: instruction budget exhausted".to_string(),
            ))
        },
    );
    // Guard BEFORE any fallible operation: if `create_table`/`set` fail (out
    // of memory; does not happen in practice) or `f.call` errors, the hook
    // is removed anyway on exit via the early `?`.
    let _guard = HookGuard(lua);

    let table = lua.create_table()?;
    table.set("cwd", lua.create_string(&input.cwd)?)?;
    table.set("selected", input.selected)?;
    table.set("selected_bytes", input.selected_bytes)?;
    table.set("entries", input.entries)?;
    table.set("tasks", input.tasks)?;

    let out: mlua::String = f.call(table)?;
    Ok(String::from_utf8_lossy(&out.as_bytes()).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lua::{Layer, LuaHost};

    fn input() -> StatusInput {
        StatusInput {
            cwd: b"mem:///d".to_vec(),
            selected: 2,
            selected_bytes: 10,
            entries: 5,
            tasks: 0,
        }
    }

    #[test]
    fn hook_paints_and_caches() {
        let h = LuaHost::new().unwrap();
        h.eval_layer(
            b"norte.ui.statusbar(function(s) return s.selected .. ' sel' end)",
            Layer::User,
        )
        .unwrap();
        assert_eq!(h.statusbar(&input()).as_deref(), Some("2 sel"));

        // Same input = cache (checks that a function with a global Lua
        // counter only runs once for the same snapshot).
        let h2 = LuaHost::new().unwrap();
        h2.eval_layer(
            b"n = 0; norte.ui.statusbar(function(s) n = n + 1 return tostring(n) end)",
            Layer::User,
        )
        .unwrap();
        assert_eq!(h2.statusbar(&input()).as_deref(), Some("1"));
        assert_eq!(h2.statusbar(&input()).as_deref(), Some("1"), "cached");
    }

    #[test]
    fn budget_exceeded_disables_without_panic() {
        let h = LuaHost::new().unwrap();
        h.eval_layer(
            b"norte.ui.statusbar(function() while true do end end)",
            Layer::User,
        )
        .unwrap();
        assert_eq!(h.statusbar(&input()), None, "exceeded -> None + disabled");
        assert_eq!(h.statusbar(&input()), None, "still disabled");
        assert!(
            h.statusbar_error().is_some(),
            "the error is kept for the bar"
        );
    }

    #[test]
    fn hostile_output_masked() {
        let h = LuaHost::new().unwrap();
        h.eval_layer(
            "norte.ui.statusbar(function() return 'a\u{202E}b' end)".as_bytes(),
            Layer::User,
        )
        .unwrap();
        let s = h.statusbar(&input()).unwrap();
        assert!(!s.contains('\u{202E}'), "no bidi: {s}");
    }

    /// Regression: a later `eval_layer` over an ALREADY-LIVE host (e.g. the
    /// `Project` layer, evaluated after resolving the TOFU modal — task 8 —
    /// on a host that was already serving `statusbar()` with the
    /// `System`/`User` layers) that redefines the hook must NOT serve the
    /// old hook's cached response if the `StatusInput` had not changed
    /// meanwhile.
    #[test]
    fn redefining_the_hook_invalidates_the_previous_cache() {
        let h = LuaHost::new().unwrap();
        h.eval_layer(
            b"norte.ui.statusbar(function() return 'v1' end)",
            Layer::User,
        )
        .unwrap();
        assert_eq!(h.statusbar(&input()).as_deref(), Some("v1"));

        h.eval_layer(
            b"norte.ui.statusbar(function() return 'v2' end)",
            Layer::Project,
        )
        .unwrap();
        assert_eq!(
            h.statusbar(&input()).as_deref(),
            Some("v2"),
            "the old hook's cache must not survive the redefinition"
        );
    }

    // ---- spec-reviewer regressions (task 7, unique hook slot) -----------

    /// Shared setup for the `run_active` regressions: an embedded `Backend`
    /// over a `MemProvider` with `mem:///a` written, ready for a `copy`
    /// that can be made slow with `faults().set_latency_per_op` (same
    /// pattern as `tests/lua_driver.rs::cancel_kills_the_script_and_its_tasks`).
    async fn backend_with_source() -> (
        norte_core::backend::Backend,
        std::sync::Arc<norte_testkit::MemProvider>,
    ) {
        use norte_vfs::Provider;
        let engine = norte_core::Engine::new();
        let mem = std::sync::Arc::new(norte_testkit::MemProvider::new());
        engine.register_provider(std::sync::Arc::clone(&mem) as std::sync::Arc<dyn Provider>);
        let vp = norte_proto::VPath::parse("mem:///a").expect("wire");
        let mut sink = mem.write(&vp).await.unwrap();
        sink.write(bytes::Bytes::from_static(b"x")).await.unwrap();
        sink.commit().await.unwrap();
        (
            norte_core::backend::Backend::Embedded(std::sync::Arc::new(engine)),
            mem,
        )
    }

    fn ctx_mem() -> crate::lua::PaneCtx {
        let vp = |w: &str| norte_proto::VPath::parse(w).expect("wire");
        crate::lua::PaneCtx {
            cwd: vp("mem:///"),
            other_cwd: vp("mem:///"),
            selection: vec![],
            current: None,
        }
    }

    /// Regression: `statusbar()` with a command in flight (latency injected
    /// into the copy — the Task has NOT finished, the run is truly "in
    /// flight", not resolved in microseconds) must NEVER execute the hook: a
    /// global Lua counter must stay at `0` (seen through the `None`, since
    /// with no prior cache the only honest output with a live run is
    /// `None`). Once the run finishes, `statusbar()` works again and
    /// actually executes the hook.
    ///
    /// Without the `run_active` guard this test is red: nothing stops
    /// `statusbar()` from calling Lua during the run, so it would return
    /// `Some("1")` instead of `None` (verified manually by removing the
    /// guard).
    /// `start_paused`: the injected latency and the probe use tokio timers —
    /// with the clock paused the runtime ADVANCES time as soon as it goes
    /// idle: deterministic and instant, with no wall-clock races (rust
    /// review).
    #[tokio::test(start_paused = true)]
    async fn statusbar_does_not_touch_lua_with_a_run_in_flight() {
        let (backend, mem) = backend_with_source().await;
        mem.faults()
            .set_latency_per_op(Some(std::time::Duration::from_millis(200)));

        let h = LuaHost::new().unwrap();
        h.eval_layer(
            b"n = 0\n\
              norte.ui.statusbar(function() n = n + 1 return tostring(n) end)\n\
              norte.command('copy', function()\n\
                norte.fs.copy('mem:///a', 'mem:///b')\n\
              end)",
            Layer::User,
        )
        .unwrap();

        let run = h
            .invoke(
                "copy",
                backend.clone(),
                ctx_mem(),
                tokio_util::sync::CancellationToken::new(),
            )
            .expect("exists");

        let probe = async {
            // Let the copy actually start (enter the injected latency)
            // before probing the bar.
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            assert_eq!(
                h.statusbar(&input()),
                None,
                "run in flight: must never execute the hook (nor touch Lua)"
            );
        };

        let (outcome, ()) = tokio::join!(run, probe);
        assert!(
            matches!(outcome, crate::lua::RunOutcome::Ok { .. }),
            "{outcome:?}"
        );

        // After the run finishes: statusbar() actually executes again (the
        // counter advances to 1 — it is the FIRST real execution).
        assert_eq!(h.statusbar(&input()).as_deref(), Some("1"));
    }

    /// Reviewer's repro: a command with a slow copy followed by a pure Lua
    /// loop; IF, while the run is in flight, something called
    /// `Lua::set_hook`/`remove_hook` (statusbar WITHOUT the guard), the
    /// driver's cancellation hook would be silently disarmed (mlua's
    /// unique hook slot) and the loop would be UNCANCELLABLE — the test
    /// uses a short timeout (`invoke_with_timeout`) so that, if the bug
    /// reappears, it fails fast with `TimedOut` instead of exhausting the
    /// default timeout (5 min).
    ///
    /// Verified red by temporarily removing the `if self.run_active.get()`
    /// from `LuaHost::statusbar`: the outcome becomes `TimedOut` (the
    /// cancellation does not arrive in time because the driver's hook was
    /// left inert).
    /// `start_paused`: see [`statusbar_does_not_touch_lua_with_a_run_in_flight`].
    #[tokio::test(start_paused = true)]
    async fn cancel_still_works_after_statusbar_during_a_run() {
        let (backend, mem) = backend_with_source().await;
        mem.faults()
            .set_latency_per_op(Some(std::time::Duration::from_millis(200)));

        let h = LuaHost::new().unwrap();
        h.eval_layer(
            b"norte.ui.statusbar(function() return 'ok' end)\n\
              norte.command('loop', function()\n\
                local ok = norte.fs.copy('mem:///a', 'mem:///b')\n\
                while true do end\n\
              end)",
            Layer::User,
        )
        .unwrap();
        // A REAL hook registered: without this, `statusbar()` shortcuts at
        // `registry.statusbar.clone()?` (None) and NEVER gets to touch
        // `Lua::set_hook`/`remove_hook` — the interference this test
        // reproduces requires the bar to actually try to run the hook.
        assert!(
            h.statusbar(&input()).is_some(),
            "precondition: the hook exists and runs cold (no run in flight)"
        );
        // Input DIFFERENT from the precondition's: if it matched, the cache
        // from the line above would serve the response without touching
        // Lua during the run, and this test would prove nothing — we need
        // the attempt to execute the hook to be REAL.
        let during_input = StatusInput {
            selected: 99,
            ..input()
        };

        let token = tokio_util::sync::CancellationToken::new();
        let run = h
            .invoke_with_timeout(
                "loop",
                backend.clone(),
                ctx_mem(),
                token.clone(),
                std::time::Duration::from_secs(2),
            )
            .expect("exists");

        let cancel = async {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            // The reviewer's point: call statusbar() WHILE the run is in
            // flight, with an input that has NO cache (forces a real
            // attempt to execute the hook). With the guard, it is a no-op
            // (`None`, without touching Lua); that is exactly what this
            // test verifies indirectly via the cancellation outcome below.
            assert_eq!(
                h.statusbar(&during_input),
                None,
                "must not touch Lua with the run in flight"
            );
            token.cancel();
        };

        let (outcome, ()) = tokio::join!(run, cancel);
        assert!(
            matches!(outcome, crate::lua::RunOutcome::Cancelled),
            "{outcome:?}"
        );
    }

    /// The statusbar hook calling an API that only exists DURING a run
    /// (`norte.fs`, installed by `install_fs` on every `invoke`, task 4/5)
    /// blows up and disables the hook. With this fix's `run_active` guard,
    /// the case "the hook actually calls `norte.fs.stat` while a run is in
    /// flight" is UNREACHABLE: `statusbar()` never executes the hook with a
    /// live run, so `norte.fs` is NEVER installed when the hook runs. What
    /// IS reachable — and what this test proves — is the generic error
    /// path: in the normal state (no run) `norte.fs` does not exist, so the
    /// call blows up indexing `nil` — a different error message from a real
    /// async call, but with the SAME outcome (disables + `statusbar_error()`
    /// with Some) as any other hook failure.
    #[test]
    fn fs_async_from_the_hook_disables() {
        let h = LuaHost::new().unwrap();
        h.eval_layer(
            b"norte.ui.statusbar(function() return norte.fs.stat('mem:///x') end)",
            Layer::User,
        )
        .unwrap();
        assert_eq!(
            h.statusbar(&input()),
            None,
            "norte.fs does not exist outside a run -> blows up -> disabled"
        );
        assert!(
            h.statusbar_error().is_some(),
            "the failure detail is kept for the bar"
        );
    }

    /// Regression (task 7 re-review): if the caller (T8 or any future one)
    /// creates a `CommandRun` and drops it WITHOUT ever polling it, the
    /// `async fn run_command`'s body NEVER starts executing — so no
    /// internal guard of that function ever runs. If `run_active` depended
    /// on a guard built INSIDE the future, it would get stuck at `true`
    /// forever (the bar frozen until the next hot-reload). `CommandRun`
    /// must turn it off structurally in its own `Drop`, without depending
    /// on it being polled.
    #[tokio::test]
    async fn command_run_dropped_without_polling_does_not_freeze_the_statusbar() {
        let (backend, _mem) = backend_with_source().await;
        let h = LuaHost::new().unwrap();
        h.eval_layer(
            b"norte.ui.statusbar(function() return 'ok' end)\n\
              norte.command('loop', function() while true do end end)",
            Layer::User,
        )
        .unwrap();

        let run = h
            .invoke(
                "loop",
                backend,
                ctx_mem(),
                tokio_util::sync::CancellationToken::new(),
            )
            .expect("exists");
        drop(run); // NEVER polled: the async fn's body never ran.

        assert_eq!(
            h.statusbar(&input()).as_deref(),
            Some("ok"),
            "a CommandRun dropped without polling must not leave run_active stuck"
        );
    }

    /// Regression (re-review 3, rust-reviewer MAJOR): the caller's (T8)
    /// natural pattern `self.run = Some(host.invoke(...))` evaluates the
    /// RHS — builds the NEW run, increments `run_active` — BEFORE dropping
    /// the OLD value that occupied the slot. With a boolean `run_active`,
    /// the old one's `Drop` (which had already finished but was still alive
    /// in the slot) would clobber the `true` just set by the new one,
    /// leaving it UNPROTECTED: `statusbar()` could touch Lua while the new
    /// run is still in flight, disarming its cancellation hook (the same
    /// underlying bug as the regressions above, but triggered by the
    /// slot's lifecycle, not by a direct call to `statusbar()`). With a
    /// counter, the new one's increment and the old one's decrement cancel
    /// out and the counter never drops to zero while the new one is alive.
    /// `start_paused`: see [`statusbar_does_not_touch_lua_with_a_run_in_flight`].
    #[tokio::test(start_paused = true)]
    async fn assigning_a_new_run_over_the_old_slot_does_not_unprotect() {
        let (backend, mem) = backend_with_source().await;
        let h = LuaHost::new().unwrap();
        h.eval_layer(
            b"norte.ui.statusbar(function() return 'ok' end)\n\
              norte.command('trivial', function() end)\n\
              norte.command('loop', function()\n\
                local ok = norte.fs.copy('mem:///a', 'mem:///b')\n\
                while true do end\n\
              end)",
            Layer::User,
        )
        .unwrap();

        // OLD run: trivial, completes RIGHT AWAY (no latency) but is kept
        // alive without dropping — polled BY REFERENCE (`&mut old`), not
        // consumed, so it can keep being held after the outcome
        // (`CommandRun` is `Unpin`, so `&mut CommandRun` is a `Future`).
        let mut old = h
            .invoke(
                "trivial",
                backend.clone(),
                ctx_mem(),
                tokio_util::sync::CancellationToken::new(),
            )
            .expect("exists");
        let outcome_old = (&mut old).await;
        assert!(matches!(outcome_old, crate::lua::RunOutcome::Ok { .. }));

        // Latency so the NEW run is truly in flight when we probe it.
        mem.faults()
            .set_latency_per_op(Some(std::time::Duration::from_millis(200)));
        let token = tokio_util::sync::CancellationToken::new();
        let new = h
            .invoke_with_timeout(
                "loop",
                backend.clone(),
                ctx_mem(),
                token.clone(),
                std::time::Duration::from_secs(2),
            )
            .expect("exists");

        // The natural pattern `self.run = Some(host.invoke(...))`: `new`
        // has already been built (RHS evaluated) above; NOW the old value
        // that occupied the slot is dropped — the order matters, it is
        // exactly what reproduces the bug with a boolean.
        drop(old);

        let probe = async {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            assert_eq!(
                h.statusbar(&input()),
                None,
                "the new run must stay protected despite the old one's Drop"
            );
            token.cancel();
        };

        let (outcome, ()) = tokio::join!(new, probe);
        assert!(
            matches!(outcome, crate::lua::RunOutcome::Cancelled),
            "{outcome:?}"
        );
    }
}
