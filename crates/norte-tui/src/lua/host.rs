//! Load the Lua host, resolve its trust and run its commands.
//!
//! It used to live in the root of the `ntc` binary — a DIFFERENT crate from
//! this lib —, so the event loop was the only place from which the host
//! could be talked to, and its tests had to live inside `main.rs`.
//!
//! The rest of `lua/` is the host itself (`api`), the command driver
//! (`driver`), the fs API (`fs`), the bar (`statusbar`) and trust
//! (`trust`). This is the glue on top: what gets loaded, in what order, and
//! what the reader is told when any of that fails.

use std::collections::VecDeque;

use tokio_util::sync::CancellationToken;

use crossterm::event::KeyCode;
use norte_core::backend::Backend;
use norte_i18n::{t, ta};
use sha2::Digest as _;

use super::{CommandRun, Layer, LuaHost, PaneCtx, StatusInput, TrustDecision, TrustStore};
use crate::app::{App, DialogOutcome, Modal, detail_for_bar, io_error_category, trust_lua_key};
use crate::config::Layers;

/// Cap of the Lua command FIFO queue (M4): with a run in flight, further
/// ones queue up to here; once full, only the notice remains.
const LUA_QUEUE_MAX: usize = 8;

/// STABLE label of a layer for `err-lua-load` (not localized: it is a
/// layer identifier, not prose).
fn lua_layer_label(layer: Layer) -> &'static str {
    match layer {
        Layer::System => "system",
        Layer::User => "user",
        Layer::Profile => "profile",
        Layer::Project => "project",
    }
}

/// Evaluates a layer on the host and routes error/warnings to the bar
/// (`err-lua-load`; with several, the last one wins the slot — fine for
/// v1). The detail is RAW diagnostic from the Lua runtime: ALWAYS through
/// `detail_for_bar` (pattern #73).
fn eval_lua_layer(app: &mut App, host: &LuaHost, source: &[u8], layer: Layer) {
    let label = lua_layer_label(layer);
    match host.eval_layer(source, layer) {
        Ok(warnings) => {
            for w in warnings {
                app.message = Some(ta(
                    "err-lua-load",
                    &[("layer", label), ("detail", &detail_for_bar(&w.detail))],
                ));
            }
        }
        Err(e) => {
            app.message = Some(ta(
                "err-lua-load",
                &[
                    ("layer", label),
                    ("detail", &detail_for_bar(&e.to_string())),
                ],
            ));
        }
    }
}

/// Reads `path` if it exists (`spawn_blocking`, rule 2): `None` = layer
/// absent.
async fn read_optional_bytes(path: std::path::PathBuf) -> std::io::Result<Option<Vec<u8>>> {
    match tokio::task::spawn_blocking(move || std::fs::read(&path)).await {
        Ok(Ok(bytes)) => Ok(Some(bytes)),
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Ok(Err(e)) => Err(e),
        // A panic while reading is OUR bug: let it blow up visibly (same
        // criterion as `config::load_async`).
        Err(e) => std::panic::resume_unwind(e.into_panic()),
    }
}

/// Loads the `init.lua` files layer by layer (ADR 0007 + 0026): system and
/// user are evaluated directly (the user's OWN config); the PROJECT one
/// (`./.norte`, the LAST layer, as in `config::standard_layers`) goes
/// through TOFU trust (`load_lua_project`, private). Loading does NOT touch
/// the `Backend` (it only evaluates code; the commands' FS arrives in
/// `invoke`). Errors/warnings go to the bar by category; a broken layer
/// does not block the rest.
///
/// Returns `None` if mlua could not even start: scripting is left
/// disabled with a notice — the TUI keeps going.
///
/// On hot-reload it is called again and the host is REBORN wholesale (a
/// `CommandRun` in flight retains the old state via its handles —
/// documented in `lua::api`); a `TrustLuaInit` pending from the previous
/// load becomes stale and is closed (its bytes are no longer what would be
/// evaluated).
pub async fn load_lua(app: &mut App, layers: &Layers) -> Option<LuaHost> {
    if matches!(app.modal, Some(Modal::TrustLuaInit { .. })) {
        app.modal = None;
        app.open_next_pending();
    }
    app.lua_pending_trust = None;

    let host = match LuaHost::new() {
        Ok(h) => h,
        Err(e) => {
            app.message = Some(ta(
                "err-lua-load",
                &[
                    ("layer", "host"),
                    ("detail", &detail_for_bar(&e.to_string())),
                ],
            ));
            return None;
        }
    };
    for &(ref dir, layer) in &layers.dirs {
        // The kind travels PER DIR (debt #75 closed): it used to be
        // inferred by position and the LABEL failed on Windows without
        // ProgramData (APPDATA stayed "system").
        match lua_of_this_layer(layer) {
            LayerLua::AfterTrust => load_lua_project(app, &host, dir.clone()).await,
            LayerLua::Ignored => {
                if matches!(read_optional_bytes(dir.join("init.lua")).await, Ok(Some(_))) {
                    app.message = Some(t("err-lua-profile-ignored"));
                }
            }
            LayerLua::Runs => match read_optional_bytes(dir.join("init.lua")).await {
                Ok(Some(bytes)) => eval_lua_layer(app, &host, &bytes, layer),
                Ok(None) => {}
                Err(e) => {
                    app.message = Some(ta(
                        "err-lua-load",
                        &[
                            ("layer", lua_layer_label(layer)),
                            ("detail", &io_error_category(&e)),
                        ],
                    ));
                }
            },
        }
    }
    Some(host)
}

/// What to do with a layer's `init.lua`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LayerLua {
    /// Evaluated without further ado: the layer belongs to the reader and
    /// nobody handed it to them.
    Runs,
    /// Only evaluated after ADR 0026's TOFU.
    AfterTrust,
    /// Never evaluated, and it says so.
    Ignored,
}

/// What to do with `layer`'s `init.lua`.
///
/// An exhaustive `match`, and NOT an `if` against `Project`, which is what
/// there used to be: the condition was `layer != Layer::Project`, so
/// `Layer::Profile` silently inherited CODE EXECUTION from the user layer
/// the moment the variant existed. It is the same negation that
/// `norte-config`'s trim came to kill, in another file, and it compiled
/// without a word. Now a new [`Layer`] variant does not compile until
/// someone decides which side it falls on.
///
/// **A profile DECLARES, it does not EXECUTE.** That is the line, and it
/// leaves it everything the spec granted it — theme, `keymap.toml`,
/// `openers.toml`, `layouts/`, favorites —, which are files the reader can
/// open and understand. `init.lua` not: the Lua host is not sandboxed
/// (the whole stdlib, `os.execute` included), the profile layer is chosen
/// from a LIST with the program already running, and the very same file
/// reached as a profile would not even pass the TOFU that IS required of
/// one from a repository.
pub(crate) const fn lua_of_this_layer(layer: Layer) -> LayerLua {
    match layer {
        Layer::System | Layer::User => LayerLua::Runs,
        Layer::Profile => LayerLua::Ignored,
        Layer::Project => LayerLua::AfterTrust,
    }
}

/// Result of the VERIFIED read of the project's `init.lua`.
enum ProjectLua {
    /// There is no `./.norte/init.lua` (or `.norte` is not a directory):
    /// nothing.
    Absent,
    /// `.norte` or `init.lua` are SYMLINKS (security criterion from T6's
    /// review): a symlink to an already-trusted project would execute
    /// content approved for ANOTHER location in a hostile context. Not
    /// loaded, with a notice.
    Symlink,
    /// Real io error (permissions, etc.).
    Io(std::io::Error),
    /// CANONICAL path + bytes read exactly once.
    Ready(std::path::PathBuf, Vec<u8>),
}

/// Checks + reads the project script, all synchronous in one block (called
/// under `spawn_blocking`): `symlink_metadata` verifies that `.norte` is a
/// REAL directory and that `init.lua` is a REGULAR file — never through a
/// symlink. The window between the check and the `read` is not zero
/// (there is no portable `O_NOFOLLOW` here), but the content READ is
/// exactly what gets approved and evaluated (anti-TOCTOU for the content;
/// the residual risk is on the path). The path is CANONICALIZED so that
/// checking and recording always use the same form (an NFC/NFD limitation
/// of the store, documented in `lua::trust`).
fn project_lua_read(dir: &std::path::Path) -> ProjectLua {
    let dir_md = match std::fs::symlink_metadata(dir) {
        Ok(md) => md,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return ProjectLua::Absent,
        Err(e) => return ProjectLua::Io(e),
    };
    if dir_md.file_type().is_symlink() {
        return ProjectLua::Symlink;
    }
    if !dir_md.is_dir() {
        return ProjectLua::Absent;
    }
    let file = dir.join("init.lua");
    let file_md = match std::fs::symlink_metadata(&file) {
        Ok(md) => md,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return ProjectLua::Absent,
        Err(e) => return ProjectLua::Io(e),
    };
    if file_md.file_type().is_symlink() {
        return ProjectLua::Symlink;
    }
    if !file_md.is_file() {
        return ProjectLua::Absent;
    }
    let canon = match std::fs::canonicalize(&file) {
        Ok(c) => c,
        Err(e) => return ProjectLua::Io(e),
    };
    match std::fs::read(&file) {
        Ok(bytes) => ProjectLua::Ready(canon, bytes),
        Err(e) => ProjectLua::Io(e),
    }
}

/// PROJECT layer (ADR 0026): checks symlinks, consults the [`TrustStore`]
/// (`state_dir()/lua-trust.toml`, `spawn_blocking`) and decides —
/// `Trusted` evaluates; `Denied` STAYS SILENT; `DeniedPathChanged` warns
/// (a silent deny, NEVER an automatic modal: reopening it on every edit of
/// an already-rejected script would end in approval by fatigue); `Unknown`
/// opens the TOFU modal, leaving the bytes pending in
/// `App::lua_pending_trust`.
async fn load_lua_project(app: &mut App, host: &LuaHost, dir: std::path::PathBuf) {
    let read = match tokio::task::spawn_blocking(move || project_lua_read(&dir)).await {
        Ok(r) => r,
        Err(e) => std::panic::resume_unwind(e.into_panic()),
    };
    let (path, bytes) = match read {
        ProjectLua::Absent => return,
        ProjectLua::Symlink => {
            app.message = Some(t("msg-lua-symlink"));
            return;
        }
        ProjectLua::Io(e) => {
            app.message = Some(ta(
                "err-lua-load",
                &[("layer", "project"), ("detail", &io_error_category(&e))],
            ));
            return;
        }
        ProjectLua::Ready(path, bytes) => (path, bytes),
    };
    let Some(state) = norte_config::dirs::state_dir() else {
        // No state dir means no store; no store means no TOFU; no TOFU
        // means the project script does NOT run (fail-closed) — with a
        // notice.
        app.message = Some(t("err-lua-no-state-dir"));
        return;
    };
    let store_path = state.join("lua-trust.toml");
    let (check_path, check_bytes) = (path.clone(), bytes.clone());
    let decision = match tokio::task::spawn_blocking(move || {
        TrustStore::open(store_path).map(|s| s.check(&check_path, &check_bytes))
    })
    .await
    {
        Ok(Ok(d)) => d,
        Ok(Err(e)) => {
            // Unreadable/corrupt store: fail-closed (it could be the trace
            // of tampering, not a benign absence — `lua::trust`).
            app.message = Some(ta(
                "err-lua-load",
                &[
                    ("layer", "project"),
                    ("detail", &detail_for_bar(&e.to_string())),
                ],
            ));
            return;
        }
        Err(e) => std::panic::resume_unwind(e.into_panic()),
    };
    match decision {
        TrustDecision::Trusted => eval_lua_layer(app, host, &bytes, Layer::Project),
        TrustDecision::Denied => {}
        TrustDecision::DeniedPathChanged => app.message = Some(t("msg-lua-denied-changed")),
        TrustDecision::Unknown => {
            if app.modal.is_some() {
                // Another modal open (only reachable on hot-reload): it is
                // neither clobbered nor queued (v1) — the next reload asks
                // again.
                return;
            }
            let hash = sha2::Sha256::digest(&bytes);
            // 16 bytes = 32 hex = 128 bits (security review M4 Lua): the
            // human compares WHAT THEY SEE — forging a 32-bit collision
            // (8 hex) takes minutes; 128 bits is impossible in practice.
            let hash_abbrev = hash.iter().take(16).fold(String::new(), |mut s, b| {
                use std::fmt::Write as _;
                let _ = write!(s, "{b:02x}");
                s
            });
            app.modal = Some(Modal::TrustLuaInit {
                // Sanitized HERE (the modal's contract: `path` already
                // ready to paint) — a path from someone else's repo can
                // carry bidi/control characters.
                path: detail_for_bar(&path.display().to_string()),
                hash_abbrev,
            });
            app.lua_pending_trust = Some((path, bytes));
        }
    }
}

/// Resolves the [`Modal::TrustLuaInit`] modal (intercepted in the run loop,
/// which is the one holding the host — decision 8 of plan H1: NOT migrated
/// to the `dialog` context): `y` trusts, `n`/Esc deny, Enter does NOT
/// decide ([`trust_lua_key`]). The decision is PERSISTED in the
/// [`TrustStore`] (`spawn_blocking`, rule 2) and, if approved, the BYTES
/// saved in `App::lua_pending_trust` are evaluated — what was approved =
/// what is evaluated (anti-TOCTOU), never a re-read from disk. A failure
/// while persisting does not block THIS session's decision (it will only
/// ask again next time): notice and carry on.
pub async fn resolve_lua_trust(app: &mut App, host: Option<&LuaHost>, code: KeyCode) {
    if app.modal.is_none() {
        return;
    }
    let allow = match trust_lua_key(code) {
        DialogOutcome::Confirmed => true,
        DialogOutcome::Cancelled => false,
        DialogOutcome::Open | DialogOutcome::Retry(_) => return,
    };
    app.modal = None;
    if let Some((path, bytes)) = app.lua_pending_trust.take() {
        let (rec_path, rec_bytes) = (path.clone(), bytes.clone());
        let record = tokio::task::spawn_blocking(move || -> std::io::Result<()> {
            let dir = norte_config::dirs::state_dir().ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::NotFound, "no state directory")
            })?;
            let mut store = TrustStore::open(dir.join("lua-trust.toml"))?;
            store.record(&rec_path, &rec_bytes, allow)
        })
        .await;
        match record {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                app.message = Some(ta(
                    "err-lua-load",
                    &[("layer", "project"), ("detail", &io_error_category(&e))],
                ));
            }
            // A panic while persisting is OUR bug: let it blow up visibly
            // (same criterion as this binary's other spawn_blocking calls).
            Err(e) => std::panic::resume_unwind(e.into_panic()),
        }
        if allow && let Some(host) = host {
            eval_lua_layer(app, host, &bytes, Layer::Project);
        }
    }
    app.open_next_pending();
}

/// Starts the Lua command `name` with the CURRENT pane snapshot as
/// `PaneCtx` (frozen: determinism over freshness). The TUI does not have
/// multi-selection yet: `selection` = the entry under the cursor (or
/// empty) — documented, same data as `current`. An unregistered command →
/// `err-lua-unknown` on the bar (not a keymap error) and `None`.
pub fn start_lua_run(
    app: &mut App,
    host: &LuaHost,
    backend: &Backend,
    name: &str,
) -> Option<(CommandRun, CancellationToken)> {
    let pane = app.focused();
    let other = &app.panes[app.target_index().unwrap_or_else(|| app.focus())];
    let current = pane.selected().map(|e| e.path.clone());
    let ctx = PaneCtx {
        cwd: pane.dir().clone(),
        other_cwd: other.dir().clone(),
        selection: current.clone().into_iter().collect(),
        current,
    };
    let token = CancellationToken::new();
    let Some(run) = host.invoke(name, backend.clone(), ctx, token.clone()) else {
        app.message = Some(ta("err-lua-unknown", &[("name", &detail_for_bar(name))]));
        return None;
    };
    Some((run, token))
}

/// Dispatches a `lua:<name>` binding: with a run in flight it QUEUES it
/// (FIFO, cap `LUA_QUEUE_MAX`; full = just the notice); if free, it starts
/// it. Without a host (mlua did not start) the command cannot exist →
/// `err-lua-unknown`.
pub fn run_lua_command(
    app: &mut App,
    lua_host: Option<&LuaHost>,
    backend: &Backend,
    name: &str,
    lua_run: &mut Option<(CommandRun, CancellationToken)>,
    lua_queue: &mut VecDeque<String>,
) {
    let Some(host) = lua_host else {
        app.message = Some(ta("err-lua-unknown", &[("name", &detail_for_bar(name))]));
        return;
    };
    if lua_run.is_some() {
        if lua_queue.len() < LUA_QUEUE_MAX {
            lua_queue.push_back(name.to_owned());
            app.message = Some(t("msg-lua-busy"));
        } else {
            // Full queue = DISCARD: say so ("queued" would be a lie).
            app.message = Some(t("msg-lua-queue-full"));
        }
        return;
    }
    *lua_run = start_lua_run(app, host, backend, name);
}

/// Recomputes the Lua bar (the `norte.ui.statusbar` hook) with the focused
/// pane's snapshot. The host caches by `PartialEq` and FREEZES with a run
/// in flight (see `lua::api`); its output already comes sanitized. A hook
/// failure (take-once) goes to the bar once and the hook stays disabled
/// until the next hot-reload.
pub fn refresh_lua_status(app: &mut App, lua_host: Option<&LuaHost>) {
    let Some(host) = lua_host else {
        app.lua_status = None;
        return;
    };
    let pane = app.focused();
    let input = StatusInput {
        cwd: pane.dir().to_wire().into_bytes(),
        selected: pane.cursor(),
        // No multi-selection: the bytes of the entry under the cursor.
        selected_bytes: pane.selected().and_then(|e| e.size).unwrap_or(0),
        entries: pane.entries().len(),
        tasks: app
            .board
            .rows()
            .iter()
            .filter(|r| !r.last.state.is_terminal())
            .count(),
    };
    app.lua_status = host.statusbar(&input);
    if let Some(detail) = host.statusbar_error() {
        app.message = Some(ta(
            "err-lua-statusbar",
            &[("detail", &detail_for_bar(&detail))],
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::{Layer, LayerLua, lua_of_this_layer};

    /// A PROFILE's `init.lua` does not execute.
    ///
    /// The Lua host is not sandboxed and the profile layer is chosen from a
    /// list with the program already running: if it ran, picking a row in
    /// the selector would be executing arbitrary code without a single
    /// dialog, which is the "privilege escalator" the spec describes. And
    /// D2's whitelist never granted it `init.lua`.
    #[test]
    fn a_profile_does_not_execute_init_lua() {
        assert_eq!(lua_of_this_layer(Layer::Profile), LayerLua::Ignored);
    }

    /// And the other three do not change: system and user belong to the
    /// reader, and the project still requires trust (ADR 0026).
    #[test]
    fn the_other_layers_do_not_change() {
        assert_eq!(lua_of_this_layer(Layer::System), LayerLua::Runs);
        assert_eq!(lua_of_this_layer(Layer::User), LayerLua::Runs);
        assert_eq!(lua_of_this_layer(Layer::Project), LayerLua::AfterTrust);
    }
}
