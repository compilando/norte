//! `LuaHost`: Lua state + command/statusbar registry. Rebuilt WHOLESALE on
//! hot-reload (never half state); an in-flight command retains the old
//! state via its cloned handles (mlua is an Rc handle).

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use mlua::{Function, Lua};
use norte_core::backend::Backend;

use super::fs::{self, PaneCtx, RunCancellers};
use super::statusbar::{self, StatusInput};

/// Source layer of an `init.lua` (ASCENDING precedence, ADR 0007). The
/// type is owned by [`crate::config`] (owner of the layer concept; debt #75):
/// here it is only re-exported for scripting.
pub use crate::config::Layer;

/// Non-fatal load warning (shown via the bar, does not abort).
#[derive(Debug, Clone)]
pub struct LuaWarning {
    /// Diagnostic payload, NEVER painted raw: the caller routes it through
    /// a Fluent key + `detail_for_bar` (pattern #73) to localize and
    /// sanitize it before showing it.
    pub detail: String,
}

/// Error loading or evaluating an `init.lua` layer.
#[derive(Debug, thiserror::Error)]
pub enum LuaLoadError {
    /// Error propagated as-is from the Lua runtime (syntax, runtime,
    /// invalid/duplicate command name — all travel as
    /// `mlua::Error::RuntimeError` from `norte.command`).
    #[error(transparent)]
    Lua(#[from] mlua::Error),

    /// [`LuaHost::eval_layer`] was called while another load was already
    /// in progress on the same host. No binding invokes `eval_layer` from
    /// inside the runtime, so today it is not reachable from Lua: the guard
    /// is defense in depth (a reentry would find the staging and the
    /// registry half-done).
    #[error("eval_layer is not reentrant: a load is already in progress")]
    Reentrant,

    /// [`LuaHost::eval_layer`] was called with some `CommandRun` of THIS
    /// host in flight: the load runs under a budget hook and mlua's hook
    /// slot is UNIQUE per instance — installing it would silently
    /// clobber the run's coroutine cancellation hook (see the
    /// `statusbar` module). Fail-closed: better to reject the load (visibly)
    /// than an uncancellable run.
    #[error("eval_layer with a run in flight: the load would clobber the cancellation hook")]
    RunInFlight,
}

/// Commands already confirmed in the registry (the layer that defined
/// them + the invocable Lua function).
#[derive(Default)]
struct Registry {
    commands: HashMap<String, (Layer, Function)>,
    /// Hook for `norte.ui.statusbar`, if some `init.lua` defined it (task 7).
    /// Unlike `commands`, it carries NO layer: the semantics are "last
    /// eval that defined it wins", with no layer precedence and no
    /// clobber warning (documented in `eval_layer`).
    statusbar: Option<Function>,
}

/// The TUI's Lua host. `!Send` — lives on the main task.
pub struct LuaHost {
    lua: Lua,
    // Rc: the driver (task 5) clones the registry handle for invoke.
    registry: Rc<RefCell<Registry>>,
    /// Reentry guard: `true` while `eval_layer` is in progress.
    loading: Cell<bool>,
    /// `true` after a statusbar hook failure (budget exhausted or
    /// runtime error): `statusbar()` returns `None` without touching Lua
    /// until this host is rebuilt wholesale (hot-reload, task 8).
    statusbar_disabled: Cell<bool>,
    /// RAW diagnostic detail of the last hook failure (task 7): the
    /// wiring (task 8) consumes it ONCE via `statusbar_error()` to
    /// paint it in the bar, sanitized.
    statusbar_error: RefCell<Option<String>>,
    /// Cache of the last invocation: same `StatusInput` (`PartialEq`) =
    /// same output, without re-invoking the script.
    statusbar_cache: RefCell<Option<(StatusInput, Option<String>)>>,
    /// Counter of in-flight commands (`driver.rs`) — a COUNTER, not a
    /// boolean (spec-review 3): `invoke_with_timeout` INCREMENTS it when
    /// building the returned `CommandRun` (on BOTH paths, including the
    /// `install_fs` error one, for symmetry with the decrement), and
    /// `CommandRun`'s `Drop` DECREMENTS it with saturation (every path:
    /// normal return, cancellation, timeout abandonment, or never even
    /// polling it).
    ///
    /// A boolean is NOT enough: the caller's natural pattern (T8) `self.run =
    /// Some(host.invoke(...))` evaluates the NEW run (which turns the
    /// protection on) BEFORE dropping the OLD run that was in the slot
    /// (which would turn it off) — with a boolean, that old Drop would
    /// clobber the `true` just set by the new one, leaving it unprotected.
    /// With a counter, the new one's increment and the old one's decrement
    /// cancel out: it only reaches zero when NO `CommandRun` (old or new)
    /// is still alive.
    ///
    /// Shared via `Rc` with `driver.rs` (see [`Self::run_active_handle`]):
    /// mlua's hook slot (`ExtraData::hook_callback`/`hook_thread`)
    /// is UNIQUE per instance — shared between the main state and ALL
    /// coroutines, even though the API exposes `Lua::set_hook` and
    /// `Thread::set_hook` as if they were independent. While some run
    /// is in flight, its coroutine has its own `Thread::set_hook` armed
    /// for cancellation (rule 3); if `statusbar()` called
    /// `Lua::set_hook`/`remove_hook` on top, the driver's C trampoline would
    /// silently disarm itself the next time it fired (a `hook_thread`
    /// mismatch) — a pure Lua loop in flight would become UNCANCELLABLE.
    /// See the `statusbar` module for the full detail.
    run_active: Rc<Cell<u32>>,
}

/// RAII: resets `loading` to `false` on leaving `eval_layer` by any
/// path (normal return or any of the early `?`s).
struct LoadingGuard<'a>(&'a Cell<bool>);

impl Drop for LoadingGuard<'_> {
    fn drop(&mut self) {
        self.0.set(false);
    }
}

/// Instruction budget for ONE load (`eval_layer`, rust-review
/// T8): a broken `init.lua` (`while true do end` at the top level) must NOT
/// be able to freeze the TUI's run loop (no draw, no Esc, terminal stuck in
/// raw mode when the process is killed). 10 M instructions is DELIBERATELY
/// generous: a legitimate load defines commands and little else (thousands
/// of instructions, not millions) — not even a baroque init.lua comes close,
/// and on current hardware it exhausts in tens of ms, not seconds.
const EVAL_BUDGET: u32 = 10_000_000;

/// Command name charset (same spirit as `agent_session`):
/// `[a-z0-9._-]{1,64}`. Used by the Lua runtime (`norte.command`) to
/// validate the name registered in `eval_layer`. SINGLE SOURCE (#88):
/// delegates to `norte_frontend::keymap::valid_lua_name` — the same charset
/// the keymap engine uses to validate `lua:<name>` bindings, so they cannot
/// drift apart. (The engine cannot depend on `norte-tui`, which depends on
/// it; the reuse direction is tui→frontend.)
fn valid_name(name: &str) -> bool {
    norte_frontend::keymap::valid_lua_name(name)
}

/// Function installed as `norte.command` outside an in-progress load: there
/// is no active staging, so any attempt to register a command from
/// outside `eval_layer` is an explicit error (avoids the subtle bug of
/// leaving the old staging captured after a failed `exec()`).
fn command_outside_load(_lua: &Lua, _args: (String, Function)) -> mlua::Result<()> {
    Err(mlua::Error::RuntimeError(
        "norte.command can only be called during init.lua loading".to_string(),
    ))
}

/// Function installed as `norte.ui.statusbar` outside an in-progress load:
/// same spirit as [`command_outside_load`] — with no active staging, an
/// attempt to register the hook outside `eval_layer` is an explicit error.
fn statusbar_outside_load(_lua: &Lua, _f: Function) -> mlua::Result<()> {
    Err(mlua::Error::RuntimeError(
        "norte.ui.statusbar can only be called during init.lua loading".to_string(),
    ))
}

/// Installs the temporary staging for `norte.ui.statusbar` for THIS load
/// (hung off `ui`, same `active` flag as `norte.command` — a
/// reference captured by the script dies with the load just like it does).
///
/// Unlike `norte.command`, redefining the hook SEVERAL times within the
/// SAME load is not an error: the last call within the staging
/// wins (a script can reassign its own hook freely while being
/// evaluated). The returned staging is merged into the real registry at
/// the end of `eval_layer`, only if the load succeeded.
fn stage_statusbar(
    lua: &Lua,
    ui: &mlua::Table,
    active: &Rc<Cell<bool>>,
) -> mlua::Result<Rc<RefCell<Option<Function>>>> {
    let staging: Rc<RefCell<Option<Function>>> = Rc::default();
    let staging_for_closure = Rc::clone(&staging);
    let active = Rc::clone(active);
    let f = lua.create_function(move |_lua, f: Function| {
        if !active.get() {
            return Err(mlua::Error::RuntimeError(
                "norte.ui.statusbar can only be called during init.lua loading".to_string(),
            ));
        }
        *staging_for_closure.borrow_mut() = Some(f);
        Ok(())
    })?;
    ui.set("statusbar", f)?;
    Ok(staging)
}

impl LuaHost {
    /// Creates a new host: full Lua stdlib (ADR 0026, no sandbox —
    /// it is user config, not third-party software) + a `norte` table with
    /// an empty `ui` subtable in globals.
    ///
    /// # Errors
    /// If mlua fails to initialize the state or install the base tables.
    pub fn new() -> mlua::Result<Self> {
        let lua = Lua::new();
        let registry = Rc::new(RefCell::new(Registry::default()));

        let norte = lua.create_table()?;
        let ui = lua.create_table()?;
        ui.set("statusbar", lua.create_function(statusbar_outside_load)?)?;
        norte.set("ui", ui)?;
        norte.set("command", lua.create_function(command_outside_load)?)?;
        lua.globals().set("norte", norte)?;

        Ok(Self {
            lua,
            registry,
            loading: Cell::new(false),
            statusbar_disabled: Cell::new(false),
            statusbar_error: RefCell::new(None),
            statusbar_cache: RefCell::new(None),
            run_active: Rc::new(Cell::new(0)),
        })
    }

    /// Evaluates the source code of an `init.lua` as belonging to `layer`.
    ///
    /// Semantics:
    /// - NOT reentrant: if a load is already in progress on this host,
    ///   returns [`LuaLoadError::Reentrant`] without touching anything.
    /// - During evaluation, `norte.command(name, f)` registers into a new
    ///   *staging* (not the real registry); an invalid or duplicate name
    ///   WITHIN THE SAME STAGING is an immediate error.
    /// - If the load fails (syntax, runtime, or `norte.command` rejected
    ///   something), the real registry stays intact — the staging is
    ///   discarded.
    /// - If the load succeeds, the staging is merged into the real registry
    ///   name by name:
    ///   - If the name did not exist, or existed in a strictly EARLIER
    ///     layer, the new definition is installed (in the second case with
    ///     a clobber [`LuaWarning`]).
    ///   - If it existed in the SAME layer (reload), it is installed without
    ///     a warning.
    ///   - If it existed in a strictly LATER layer (e.g. `System` is
    ///     re-evaluated after `User` already defined the same name), the new
    ///     definition is IGNORED — precedence never reverses — and a
    ///     [`LuaWarning`] is emitted explaining the discard.
    /// - After evaluating (success or error), the global `norte.command`
    ///   slot points again to a function that returns an error if called
    ///   outside a load. In addition, this call's own closure is
    ///   invalidated by a shared flag: if the script captured a
    ///   reference (`local c = norte.command`) and invokes it AFTER
    ///   `eval_layer` returns (e.g. from the body of an already-registered
    ///   command), the call still fails — it never writes to an orphaned
    ///   staging.
    /// - `norte.ui.statusbar(f)` (task 7) follows the same staging/`active`
    ///   flag as `norte.command` (dies with the load the same way), but its
    ///   merge is SIMPLER: no layers, no warnings. If this load called
    ///   `norte.ui.statusbar`, its function simply clobbers whatever there
    ///   was (from this same layer or another); if it did not call it, the
    ///   previous hook (if any) survives intact. In other words: "the last
    ///   `eval_layer` that defines the hook wins", regardless of layer
    ///   order.
    ///
    /// # Precondition (rust-review T8)
    /// No `CommandRun` of THIS host in flight: the load runs under an
    /// instruction budget (`EVAL_BUDGET`, `Lua::set_hook`) and mlua's hook
    /// slot is UNIQUE per instance (see `statusbar.rs`) —
    /// installing it would disarm the run's cancellation hook. This is
    /// CHECKED (`run_active`, fail-closed → [`LuaLoadError::RunInFlight`]),
    /// not just documented. The TUI's callers satisfy it almost always by
    /// construction: `load_lua` evaluates over a freshly-born host, and a
    /// run in flight across a hot-reload retains the OLD host (a different
    /// instance); the residual case (the FIFO queue starts a run on the
    /// new host while the TOFU modal is still open) falls here with a
    /// visible error instead of leaving an uncancellable run.
    ///
    /// # Errors
    /// Any Lua syntax or runtime error — including the ones
    /// `norte.command` generates for invalid or duplicate names, and the
    /// exhausted load budget (`EVAL_BUDGET`: a `while true do
    /// end` at the top level dies with an error, never freezes the TUI) —,
    /// [`LuaLoadError::Reentrant`] if a load is already in progress, and
    /// [`LuaLoadError::RunInFlight`] if a run is in flight.
    pub fn eval_layer(&self, source: &[u8], layer: Layer) -> Result<Vec<LuaWarning>, LuaLoadError> {
        if self.loading.get() {
            return Err(LuaLoadError::Reentrant);
        }
        if self.run_active.get() != 0 {
            return Err(LuaLoadError::RunInFlight);
        }
        self.loading.set(true);
        let _guard = LoadingGuard(&self.loading);

        let staging: Rc<RefCell<Vec<(String, Function)>>> = Rc::new(RefCell::new(Vec::new()));
        // "Active load session" flag: the closure installed below
        // checks it on every call, not just the global slot. This way, a
        // reference captured by the script (`local c = norte.command`) and
        // invoked later — e.g. from the body of an already-registered
        // command — dies with the load just like the global slot.
        let active = Rc::new(Cell::new(true));

        let staging_for_closure = Rc::clone(&staging);
        let active_for_closure = Rc::clone(&active);
        let command_fn = self
            .lua
            .create_function(move |_lua, (name, f): (String, Function)| {
                if !active_for_closure.get() {
                    return Err(mlua::Error::RuntimeError(
                        "norte.command can only be called during init.lua loading".to_string(),
                    ));
                }
                if !valid_name(&name) {
                    return Err(mlua::Error::RuntimeError(format!(
                        "invalid command name: {name:?} (expected [a-z0-9._-]{{1,64}})"
                    )));
                }
                let mut staging = staging_for_closure.borrow_mut();
                if staging.iter().any(|(n, _)| n == &name) {
                    return Err(mlua::Error::RuntimeError(format!(
                        "command {name} duplicated in the same layer"
                    )));
                }
                staging.push((name, f));
                Ok(())
            })
            .map_err(LuaLoadError::Lua)?;

        let norte: mlua::Table = self.lua.globals().get("norte").map_err(LuaLoadError::Lua)?;
        norte
            .set("command", command_fn)
            .map_err(LuaLoadError::Lua)?;

        // Staging for the statusbar hook (task 7), see `stage_statusbar`.
        let ui: mlua::Table = norte.get("ui").map_err(LuaLoadError::Lua)?;
        let statusbar_staging =
            stage_statusbar(&self.lua, &ui, &active).map_err(LuaLoadError::Lua)?;

        let layer_name = match layer {
            Layer::System => "init.lua (system)",
            Layer::User => "init.lua (user)",
            Layer::Profile => "init.lua (profile)",
            Layer::Project => "init.lua (project)",
        };
        // Load budget (rust-review T8, `EVAL_BUDGET`): the hook
        // ERRORS on its first firing and the chunk dies with a load error — a
        // broken init.lua never freezes the run loop. RAII guard (the SAME
        // HookGuard from statusbar.rs, one single piece): remove_hook no
        // matter what, even if exec() errors. Installing it is safe because
        // there is no run in flight (checked above: the hook slot is unique
        // per instance).
        let exec_result = {
            self.lua.set_hook(
                mlua::HookTriggers::new().every_nth_instruction(EVAL_BUDGET),
                |_, _| {
                    Err(mlua::Error::RuntimeError(
                        "init.lua: load instruction budget exhausted".to_string(),
                    ))
                },
            );
            let _guard = statusbar::HookGuard(&self.lua);
            self.lua.load(source).set_name(layer_name).exec()
        };

        // No matter what happens: (1) this call's closure stops accepting
        // commands even if it keeps a live reference (shared Rc); (2)
        // the global `norte.command` slot points again to a function
        // that rejects any call outside an in-progress load.
        //
        // NOTE: we build `restore` as a `Result` WITHOUT propagating it here
        // (no `?` in this zone) so as not to mask `exec_result` — if
        // both fail, the real load's error is the one that matters.
        active.set(false);
        let restore = self
            .lua
            .create_function(command_outside_load)
            .and_then(|f| norte.set("command", f));
        let restore_statusbar = self
            .lua
            .create_function(statusbar_outside_load)
            .and_then(|f| ui.set("statusbar", f));
        exec_result.map_err(LuaLoadError::Lua)?;
        restore.map_err(LuaLoadError::Lua)?;
        restore_statusbar.map_err(LuaLoadError::Lua)?;

        // INVARIANT: from here until `registry` is released, Lua is never
        // called (neither `exec` nor invoking a `Function`) — the registry's
        // mutable borrow must be free again before touching the runtime, or
        // a reentry would find it already borrowed.
        let mut warnings = Vec::new();
        let mut registry = self.registry.borrow_mut();
        for (name, f) in staging.borrow_mut().drain(..) {
            match registry.commands.get(&name) {
                Some((prev_layer, _)) if *prev_layer > layer => {
                    // An earlier layer (e.g. System re-evaluated) cannot
                    // clobber a later one already established (e.g. User):
                    // precedence never reverses. Discarded with a warning.
                    warnings.push(LuaWarning {
                        detail: format!("command {name} ignored: already defined by a later layer"),
                    });
                }
                Some((prev_layer, _)) if *prev_layer < layer => {
                    warnings.push(LuaWarning {
                        detail: format!("command {name} redefined by a later layer"),
                    });
                    registry.commands.insert(name, (layer, f));
                }
                // `None` (new name) or same layer (reload): installs without
                // a warning.
                _ => {
                    registry.commands.insert(name, (layer, f));
                }
            }
        }
        // Merge of the statusbar hook (task 7): if THIS load defined it,
        // it simply clobbers the previous one — unlike `commands`, there is
        // no layer precedence or warning here; "last eval that defines it
        // wins" (documented in `eval_layer`'s rustdoc). If this layer did
        // not call `norte.ui.statusbar`, the previous hook (from another
        // layer) survives.
        //
        // The `statusbar()` cache is invalidated when the hook changes:
        // without this, a later `eval_layer` over an ALREADY-LIVE host (e.g.
        // the `Project` layer, evaluated after resolving the TOFU modal —
        // task 8 — on a host that was already serving `statusbar()` with the
        // `System`/`User` layers) could return the OLD hook's cached
        // response if the `StatusInput` had not changed meanwhile.
        if let Some(f) = statusbar_staging.borrow_mut().take() {
            registry.statusbar = Some(f);
            *self.statusbar_cache.borrow_mut() = None;
        }

        Ok(warnings)
    }

    /// Names of the registered commands, sorted.
    #[must_use]
    pub fn commands(&self) -> Vec<String> {
        let registry = self.registry.borrow();
        let mut names: Vec<String> = registry.commands.keys().cloned().collect();
        names.sort();
        names
    }

    /// Paints the active `init.lua`'s statusbar hook with the `input`
    /// snapshot, if one is registered (task 7).
    ///
    /// Fast path: if the hook is disabled (previous failure) or none is
    /// registered, immediate `None` without touching Lua. If `input` is
    /// EQUAL (`PartialEq`) to the last successful call's, the cached
    /// response is returned without re-invoking the script — meant to be
    /// called on every render pass.
    ///
    /// **An in-flight command (`run_active != 0`) FREEZES the bar:** while
    /// ANY `CommandRun` from `driver.rs` is still alive (a counter, not a
    /// boolean — see the `run_active` field), this function NEVER touches
    /// Lua — not even remotely `set_hook`/`remove_hook` — it returns the
    /// cache if `input` matches, or `None` otherwise. mlua's hook slot is
    /// UNIQUE per instance (shared between the main state and ALL
    /// coroutines); overlapping it with the in-flight run's cancellation
    /// `Thread::set_hook` would silently disarm it — see the `statusbar`
    /// module and task 7's ADR/spec-review for the full detail.
    ///
    /// The caller (T8): drop the `CommandRun` as soon as you have its
    /// `RunOutcome` — while you keep it alive (even once it has already
    /// resolved), the bar stays frozen.
    ///
    /// The real call (only if there is NO run in flight) runs under an
    /// instruction budget (`statusbar::call_hook`) and is SYNCHRONOUS:
    /// if the budget runs out, the script blows up at runtime, or it returns
    /// something that does not coerce to a string, the hook is DISABLED for
    /// the rest of this host's life (until the next hot-reload, which
    /// rebuilds the `LuaHost` wholesale) and this call returns `None`. The
    /// failure detail is available once via
    /// [`Self::statusbar_error`].
    ///
    /// On success, the output goes through `crate::app::detail_for_bar` —
    /// never raw bidi/control characters, nor a bar overflowed by a long
    /// string.
    #[must_use]
    pub fn statusbar(&self, input: &StatusInput) -> Option<String> {
        if self.statusbar_disabled.get() {
            return None;
        }
        if let Some((prev_input, prev_out)) = self.statusbar_cache.borrow().as_ref()
            && prev_input == input
        {
            return prev_out.clone();
        }
        if self.run_active.get() != 0 {
            // Some run in flight (counter != 0): NEVER touch Lua (see
            // the rustdoc above and the `statusbar` module). With no cache
            // matching (checked right above), the only honest thing is
            // `None` — the bar freezes.
            return None;
        }
        // Borrow released BEFORE calling Lua (same invariant as
        // `command_fn`/`eval_layer`).
        let f = self.registry.borrow().statusbar.clone()?;
        match statusbar::call_hook(&self.lua, &f, input) {
            Ok(raw) => {
                let out = Some(crate::app::detail_for_bar(&raw));
                *self.statusbar_cache.borrow_mut() = Some((input.clone(), out.clone()));
                out
            }
            Err(e) => {
                // Disabled: NOT cached (documented — `statusbar()`
                // goes back to returning `None` directly next time, without
                // going through the cache).
                self.statusbar_disabled.set(true);
                *self.statusbar_error.borrow_mut() = Some(e.to_string());
                None
            }
        }
    }

    /// RAW diagnostic detail of the last statusbar hook failure, if
    /// there is one. CONSUMES (`take`): the wiring (task 8) paints it in the
    /// bar, sanitized, ONLY once.
    pub fn statusbar_error(&self) -> Option<String> {
        self.statusbar_error.borrow_mut().take()
    }

    /// FOR THE CRATE'S TESTS ONLY: installs `norte.fs`/`norte.pane`/
    /// `norte.ui.message` with fresh cancellers/messages (no driver, no
    /// token: nothing cancels) and evaluates `src` as an async chunk. The
    /// real execution path is `invoke` (task 5) — same `install_fs`, this
    /// helper just skips the driver in binding tests.
    ///
    /// Chunk return conversion: integer → that `i64`; `true` → 1;
    /// nil/nothing/other → 0.
    ///
    /// # Errors
    /// Any error installing the bindings or evaluating the chunk (syntax
    /// or runtime).
    #[doc(hidden)]
    pub async fn run_script_for_test(
        &self,
        backend: Backend,
        ctx: PaneCtx,
        src: &[u8],
    ) -> Result<i64, LuaLoadError> {
        let cancellers: RunCancellers = Rc::default();
        let messages: Rc<RefCell<Vec<String>>> = Rc::default();
        // The helper is a complete run: its bindings CLOSE when it ends
        // (same contract as `invoke`) — a stash from here also dies.
        let closed: Rc<Cell<bool>> = Rc::default();
        fs::install_fs(
            &self.lua,
            backend,
            ctx,
            cancellers,
            messages,
            Rc::clone(&closed),
        )?;
        let result: mlua::Result<mlua::Value> = self.lua.load(src).eval_async().await;
        closed.set(true);
        Ok(match result? {
            mlua::Value::Integer(i) => i,
            mlua::Value::Boolean(true) => 1,
            _ => 0,
        })
    }

    /// Retrieves the `Function` registered under `name`, if it exists. Used
    /// by the driver (`invoke`) — and the tests, to invoke a closure
    /// captured from a closed load. The registry's borrow is RELEASED before
    /// returning (invariant: never call Lua with the registry borrowed).
    pub(super) fn command_fn(&self, name: &str) -> Option<Function> {
        self.registry
            .borrow()
            .commands
            .get(name)
            .map(|(_, f)| f.clone())
    }

    /// Cloned handle of the Lua state (mlua is a cheap `Rc` handle). For
    /// the driver: instruction hook + `install_fs` per invocation.
    pub(super) fn lua_handle(&self) -> Lua {
        self.lua.clone()
    }

    /// Shared handle (same `Rc`, not a copy) of the `run_active`
    /// counter (see the field). The driver INCREMENTS it when building a
    /// `CommandRun` and DECREMENTS it in its `Drop` — the same `Rc` so that
    /// `statusbar()` sees the real state, not a frozen copy.
    pub(super) fn run_active_handle(&self) -> Rc<Cell<u32>> {
        Rc::clone(&self.run_active)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host() -> LuaHost {
        LuaHost::new().expect("lua starts")
    }

    #[test]
    fn registers_and_lists_commands() {
        let h = host();
        let w = h
            .eval_layer(b"norte.command('sel-up', function() end)", Layer::User)
            .expect("eval");
        assert!(w.is_empty());
        assert_eq!(h.commands(), vec!["sel-up".to_string()]);
    }

    #[test]
    fn invalid_name_is_a_load_error() {
        let h = host();
        // Uppercase, spaces, empty, >64: out (charset [a-z0-9._-]{1,64}).
        for bad in [
            "'Mal'",
            "'with space'",
            "''",
            &format!("'{}'", "a".repeat(65)),
        ] {
            let src = format!("norte.command({bad}, function() end)");
            assert!(h.eval_layer(src.as_bytes(), Layer::User).is_err(), "{bad}");
        }
    }

    #[test]
    fn duplicate_in_the_same_layer_is_an_error() {
        let h = host();
        let src = b"norte.command('x', function() end)\nnorte.command('x', function() end)";
        assert!(h.eval_layer(src, Layer::User).is_err());
    }

    #[test]
    fn later_layer_clobbers_with_warning() {
        let h = host();
        h.eval_layer(b"norte.command('x', function() end)", Layer::System)
            .expect("system");
        let w = h
            .eval_layer(b"norte.command('x', function() end)", Layer::User)
            .expect("user");
        assert_eq!(w.len(), 1, "clobber warning");
        assert_eq!(h.commands().len(), 1);
    }

    #[test]
    fn an_evaluation_error_does_not_poison_the_host() {
        let h = host();
        assert!(h.eval_layer(b"this is not lua (", Layer::System).is_err());
        h.eval_layer(b"norte.command('ok', function() end)", Layer::User)
            .expect("the next layer loads");
        assert_eq!(h.commands(), vec!["ok".to_string()]);
    }

    #[test]
    fn captured_reference_to_staging_dies_with_the_load() {
        let h = host();
        h.eval_layer(
            b"local c = norte.command\n\
              norte.command('trigger', function() c('ghost', function() end) end)",
            Layer::User,
        )
        .expect("load ok");

        let trigger = h.command_fn("trigger").expect("trigger registered");
        let result: mlua::Result<()> = trigger.call(());
        assert!(
            result.is_err(),
            "the captured reference to norte.command must fail after the load closes"
        );
        assert!(
            !h.commands().contains(&"ghost".to_string()),
            "must not sneak into the registry"
        );
    }

    #[test]
    fn earlier_layer_reevaluated_does_not_clobber_the_later_one() {
        let h = host();
        h.eval_layer(b"norte.command('x', function() end)", Layer::User)
            .expect("user defines x first");
        let before = h.command_fn("x").expect("x registered by User");

        let w = h
            .eval_layer(b"norte.command('x', function() end)", Layer::System)
            .expect("system is re-evaluated afterwards, not a load error");
        assert_eq!(
            w.len(),
            1,
            "must warn that the redefinition from an earlier layer is ignored"
        );

        let after = h.command_fn("x").expect("x is still registered");
        assert_eq!(
            before, after,
            "User's (later) definition cannot be clobbered by System (earlier)"
        );
        assert_eq!(h.commands().len(), 1);
    }

    #[test]
    fn reload_of_the_same_layer_clobbers_without_warning() {
        let h = host();
        h.eval_layer(b"norte.command('x', function() end)", Layer::User)
            .expect("first load");
        let w = h
            .eval_layer(b"norte.command('x', function() end)", Layer::User)
            .expect("reload of the same layer");
        assert!(w.is_empty(), "reloading the same layer must not warn");
        assert_eq!(h.commands().len(), 1);
    }

    #[test]
    fn norte_command_via_global_slot_after_the_load_fails() {
        let h = host();
        h.eval_layer(
            b"norte.command('trigger2', function() norte.command('ghost2', function() end) end)",
            Layer::User,
        )
        .expect("load ok");

        let trigger = h.command_fn("trigger2").expect("trigger2 registered");
        let result: mlua::Result<()> = trigger.call(());
        assert!(
            result.is_err(),
            "norte.command (via the global slot) outside a load must fail"
        );
        assert!(!h.commands().contains(&"ghost2".to_string()));
    }

    #[test]
    fn valid_name_with_full_charset_and_max_length() {
        let h = host();
        // Covers lowercase, digit, '.', '_' and '-'; exactly 64 bytes.
        let name: String = "a.b_c-9".chars().cycle().take(64).collect();
        assert_eq!(name.len(), 64);

        let src = format!("norte.command('{name}', function() end)");
        let w = h
            .eval_layer(src.as_bytes(), Layer::User)
            .expect("full charset and length 64 are valid");
        assert!(w.is_empty());
        assert!(h.commands().contains(&name));
    }

    /// MAJOR (rust-review T8): an `init.lua` with an infinite loop must NOT
    /// be able to freeze the load (it would run in the TUI's run loop: no
    /// draw, no Esc, terminal stuck in raw mode when the process is killed).
    /// `eval_layer`'s instruction budget kills it with a load error; the
    /// host remains usable afterward.
    #[test]
    fn init_lua_with_infinite_loop_does_not_freeze_the_load() {
        let h = host();
        assert!(
            h.eval_layer(b"while true do end", Layer::User).is_err(),
            "exhausted budget = load error, never a hang"
        );
        h.eval_layer(b"norte.command('ok', function() end)", Layer::User)
            .expect("the host remains usable after exhausting the budget");
        assert_eq!(h.commands(), vec!["ok".to_string()]);
    }

    #[test]
    fn eval_layer_is_not_reentrant() {
        let h = host();
        // There is no binding today that triggers this from inside Lua; the
        // state is forced directly to test the guard itself.
        h.loading.set(true);
        let err = h.eval_layer(b"norte.command('x', function() end)", Layer::User);
        assert!(matches!(err, Err(LuaLoadError::Reentrant)));
        h.loading.set(false);
    }

    /// The "no run in flight" precondition is CHECKED (fail-closed): with
    /// `run_active != 0`, loading would clobber the run's cancellation hook —
    /// it is rejected with a visible error instead of leaving an
    /// uncancellable run.
    #[test]
    fn eval_layer_with_run_in_flight_is_rejected() {
        let h = host();
        h.run_active.set(1);
        let err = h.eval_layer(b"norte.command('x', function() end)", Layer::User);
        assert!(matches!(err, Err(LuaLoadError::RunInFlight)));
        h.run_active.set(0);
        h.eval_layer(b"norte.command('x', function() end)", Layer::User)
            .expect("with no run in flight, the load passes again");
    }
}
