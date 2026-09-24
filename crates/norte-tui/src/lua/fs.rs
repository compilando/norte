//! `norte.fs`/`norte.pane`/`norte.ui.message` bindings. Paths are Lua BYTE
//! STRINGS on input and output (rule 1: zero UTF-8 assumption). Relative
//! ones resolve against `ctx.cwd`. Every mutation is an engine Task
//! (journal, policy and undo). Protocol errors → `nil, key` (Lua
//! convention; the key is the STABLE one from [`crate::app::error_key`],
//! never localized text).
//!
//! Documented deviation from the spec (§ API v1): `norte.fs.mkdir` is NOT
//! exposed — neither `Backend` nor `Engine` has mkdir today (verified
//! 2026-07-17); exposing it here would require logic in the frontend (rule
//! 7) or a shortcut outside the journal (rule 4). It is withdrawn from v1
//! and the spec is updated in task 9.
//!
//! Shared state: `cancellers` and `messages` are `Rc<RefCell<…>>` that the
//! closures share with the driver. Borrows are ALWAYS punctual (push and
//! release) and never held across a call into Lua nor across an `.await` —
//! same invariant as `api.rs`'s registry.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use mlua::{Lua, MultiValue, Value};
use norte_core::TransferOptions;
use norte_core::backend::{Backend, TaskCanceller};
use norte_proto::{DeleteMode, Entry, EntryKind, Error, Scheme, Segment, TaskState, VPath};

use crate::app::error_key;

/// Snapshot of the panes' state at the moment the command is INVOKED
/// (frozen: the UI keeps mutating while the script runs; determinism over
/// freshness).
#[derive(Debug, Clone)]
pub struct PaneCtx {
    /// Directory of the focused pane (base for relative paths).
    pub cwd: VPath,
    /// Directory of the OTHER pane.
    pub other_cwd: VPath,
    /// Marked selection in the focused pane.
    pub selection: Vec<VPath>,
    /// Entry under the cursor, if any.
    pub current: Option<VPath>,
}

/// Cancellers of the Tasks launched by THIS run (the driver cancels all of
/// them if the user aborts — rule 3).
pub type RunCancellers = Rc<RefCell<Vec<TaskCanceller>>>;

/// Cap on messages accumulated per run: a looping script does not grow the
/// queue without bound. Once reached, further ones are DISCARDED and the
/// last one accumulated is replaced with `"…"` as a visible overflow mark
/// (the length never exceeds the cap).
const MESSAGES_MAX: usize = 64;

/// Lua bytes → [`VPath`].
///
/// - With `://` = ABSOLUTE: if the bytes are UTF-8 and parse as wire
///   ([`VPath::parse`], percent-encoding), that is the canonical
///   interpretation (so the `path` that `list` returns round-trips). If not
///   (e.g. the script concatenated a `name` with raw non-UTF8 bytes), it is
///   parsed at the BYTE level: scheme/authority in UTF-8, raw segments
///   WITHOUT percent-decoding.
/// - Without `://` = RELATIVE to `base`: the bytes are split on `/` and each
///   segment goes in RAW (no percent-decoding — a name with a literal `%`
///   is not corrupted).
///
/// Assumed and documented ambiguity (encoding review M4 Lua): a UTF-8
/// absolute path is interpreted as wire, so a name that CONTAINS valid
/// percent-escapes gets decoded — and it need not be hostile: a normal
/// UTF-8 downloads file name (`report%20final.pdf`) concatenated as
/// absolute (`cwd .. '/' .. name`) would decode to
/// `report final.pdf` and operate on the WRONG file. The safe path for
/// ABSOLUTE paths is `entry.path` (the complete wire form returned by
/// `list`/`stat`/`selection`: an exact round-trip); the raw `name` is for
/// RELATIVE use (without `://`, where it is never decoded).
///
/// The POSIX forms do NOT exist: neither `.`/`..` (a [`Segment`] rejects
/// them — never traversal) nor `/abs` with a leading slash (that would be
/// an empty segment = invalid). For an absolute path, use the complete wire
/// form or build on `norte.pane.cwd()`/`other_cwd()`.
fn to_vpath(base: &VPath, raw: &[u8]) -> Result<VPath, &'static str> {
    let invalid = || error_key(&Error::InvalidPath);
    let sep = raw.windows(3).position(|w| w == b"://");
    let Some(sep) = sep else {
        // Relative: each raw segment hung off `base`.
        if raw.is_empty() {
            return Err(invalid());
        }
        let mut path = base.clone();
        for seg in raw.split(|&b| b == b'/') {
            path = path.join(Segment::new(seg).map_err(|_| invalid())?);
        }
        return Ok(path);
    };
    if let Ok(s) = std::str::from_utf8(raw)
        && let Ok(p) = VPath::parse(s)
    {
        return Ok(p);
    }
    parse_raw_absolute(raw, sep).ok_or_else(invalid)
}

/// Absolute form at the BYTE level: `scheme://[authority]/seg/…` with raw
/// segments (fallback of [`to_vpath`] when the wire form does not apply).
fn parse_raw_absolute(raw: &[u8], sep: usize) -> Option<VPath> {
    let scheme = Scheme::new(std::str::from_utf8(&raw[..sep]).ok()?).ok()?;
    let rest = &raw[sep + 3..];
    let (auth_raw, path_raw) = match rest.iter().position(|&b| b == b'/') {
        Some(i) => (&rest[..i], Some(&rest[i + 1..])),
        None => (rest, None),
    };
    let authority = if auth_raw.is_empty() {
        None
    } else {
        Some(norte_proto::Authority::new(std::str::from_utf8(auth_raw).ok()?).ok()?)
    };
    let mut path = VPath::root(scheme, authority);
    if let Some(path_raw) = path_raw
        && !path_raw.is_empty()
    {
        for seg in path_raw.split(|&b| b == b'/') {
            path = path.join(Segment::new(seg).ok()?);
        }
    }
    Some(path)
}

/// Lua success return: a single value.
fn ok_mv(v: Value) -> MultiValue {
    MultiValue::from_iter([v])
}

/// Lua error return: `nil, key` (standard convention; the key is stable,
/// see [`error_key`]).
fn err_mv(lua: &Lua, key: &str) -> mlua::Result<MultiValue> {
    Ok(MultiValue::from_iter([
        Value::Nil,
        Value::String(lua.create_string(key)?),
    ]))
}

/// `kind` as a stable string for Lua.
fn kind_str(kind: EntryKind) -> &'static str {
    match kind {
        EntryKind::File => "file",
        EntryKind::Dir => "dir",
        EntryKind::Symlink => "symlink",
        EntryKind::Other => "other",
    }
}

/// An [`Entry`] as a Lua table: `name` = RAW bytes of the last segment,
/// `path` = complete wire form (ASCII byte string), `kind`, `size` (or nil).
fn entry_table(lua: &Lua, e: &Entry) -> mlua::Result<mlua::Table> {
    let t = lua.create_table()?;
    // A provider's root has no last segment: name = "" honestly.
    let name: &[u8] = e.path.file_name().map_or(b"", Segment::as_bytes);
    t.set("name", lua.create_string(name)?)?;
    t.set("path", lua.create_string(e.path.to_wire().as_bytes())?)?;
    t.set("kind", kind_str(e.kind))?;
    if let Some(size) = e.size {
        // Lua 5.4 uses signed 64-bit integers; a size that does not fit
        // (absurd but possible with a hostile provider) is omitted (nil =
        // unknown) rather than lying with a truncated number.
        if let Ok(size) = i64::try_from(size) {
            t.set("size", size)?;
        }
    }
    Ok(t)
}

/// Terminal outcome of a Task → the mutation's Lua return. All keys come
/// from [`error_key`] — zero duplicated vocabulary literals.
fn finish(lua: &Lua, state: &TaskState) -> mlua::Result<MultiValue> {
    match state {
        TaskState::Completed => Ok(ok_mv(Value::Boolean(true))),
        TaskState::Cancelled => err_mv(lua, error_key(&Error::Cancelled)),
        TaskState::Failed { error } => err_mv(lua, error_key(error)),
        // `join` only returns terminal states; a newer protocol state
        // (`Unknown`) or an impossible one falls to err-unknown, never a
        // panic.
        _ => err_mv(lua, error_key(&Error::Unknown)),
    }
}

/// Installs `norte.fs`, `norte.pane` and `norte.ui.message` on the global
/// `norte` table that ALREADY exists (`LuaHost::new` creates it; `ui`
/// already exists and here only gains `message`). Called PER INVOCATION
/// (the `ctx` snapshot and the run channels change every time); reinstalling
/// clobbers the previous tables.
///
/// **API advice for scripts** (the `%` ambiguity, see [`to_vpath`]): to
/// refer to an entry by its ABSOLUTE form always use `entry.path`
/// (complete wire, exact round-trip); `entry.name` (raw bytes) is for
/// building RELATIVE paths — concatenating it into an absolute one would
/// decode a literal `%` in the name (`report%20final.pdf`) toward another
/// file.
///
/// `messages` accumulates the run's `norte.ui.message(s)` (bytes → lossy
/// String, capped at `MESSAGES_MAX`); the CONSUMER (driver, task 8) dumps
/// them to the bar by passing them through `detail_for_bar` (mask + cap) —
/// nothing is sanitized here, only accumulated.
///
/// **Orphan Task window (#74, `Backend::Remote` only) — CLOSED:** each
/// mutation's canceller is registered after the submit RPC returns; if the
/// driver ABANDONS the run's future (hard timeout / end of grace) with that
/// submit in flight, dropping the binding fires the backend's guard, which
/// sends `rpc.cancel {id}` (pattern #72) — the daemon's dispatch dies
/// PRE-effect and the Task never comes into being. Minimal residual window:
/// if the dispatch had already finished by the time the cancel arrives, the
/// Task exists but is orphaned only of its canceller — it is still under
/// journal/policy/undo and can be cancelled by hand from the tasks panel.
///
/// **Stash hazard — CLOSED by the `closed` flag:** a script can save
/// `norte.fs.copy` in a global and call it in a LATER run; that stale
/// reference would push cancellers onto the old run and resolve relatives
/// against a stale frozen `ctx`. The driver installs FRESH bindings on
/// every `invoke` (this function clobbers the tables) and, in addition,
/// every `norte.fs` binding checks `closed` ON ENTRY: if the run that
/// created it has already ended (the driver ALWAYS sets it to `true` on
/// exit, RAII guard), the binding returns `nil, err-unsupported`
/// ("binding of a closed run") — same pattern as `norte.command`'s flag in
/// `api.rs`. The `norte.pane`/`norte.ui.message` bindings do not check it:
/// they are a frozen snapshot / an accumulator that dies with the run —
/// stale but harmless (no effects on the FS nor on the cancellers).
#[expect(clippy::too_many_lines, reason = "one-by-one binding wiring, no logic")]
pub(crate) fn install_fs(
    lua: &Lua,
    backend: Backend,
    ctx: PaneCtx,
    cancellers: RunCancellers,
    messages: Rc<RefCell<Vec<String>>>,
    closed: Rc<Cell<bool>>,
) -> mlua::Result<()> {
    let norte: mlua::Table = lua.globals().get("norte")?;
    let fs = lua.create_table()?;

    // --- Reads ------------------------------------------------------------
    {
        let backend = backend.clone();
        let cwd = ctx.cwd.clone();
        let closed = Rc::clone(&closed);
        fs.set(
            "list",
            lua.create_async_function(move |lua, path: mlua::String| {
                let backend = backend.clone();
                let cwd = cwd.clone();
                let closed = Rc::clone(&closed);
                async move {
                    // Binding of a closed run (stash): dead, see rustdoc.
                    if closed.get() {
                        return err_mv(&lua, error_key(&Error::Unsupported));
                    }
                    let dir = match to_vpath(&cwd, &path.as_bytes()) {
                        Ok(p) => p,
                        Err(key) => return err_mv(&lua, key),
                    };
                    match backend.list(&dir).await {
                        Ok(entries) => {
                            let t = lua.create_table()?;
                            for (i, e) in entries.iter().enumerate() {
                                t.set(i + 1, entry_table(&lua, e)?)?;
                            }
                            Ok(ok_mv(Value::Table(t)))
                        }
                        Err(e) => err_mv(&lua, error_key(&e)),
                    }
                }
            })?,
        )?;
    }
    {
        let backend = backend.clone();
        let cwd = ctx.cwd.clone();
        let closed = Rc::clone(&closed);
        fs.set(
            "stat",
            lua.create_async_function(move |lua, path: mlua::String| {
                let backend = backend.clone();
                let cwd = cwd.clone();
                let closed = Rc::clone(&closed);
                async move {
                    // Binding of a closed run (stash): dead, see rustdoc.
                    if closed.get() {
                        return err_mv(&lua, error_key(&Error::Unsupported));
                    }
                    let p = match to_vpath(&cwd, &path.as_bytes()) {
                        Ok(p) => p,
                        Err(key) => return err_mv(&lua, key),
                    };
                    match backend.stat(&p).await {
                        Ok(e) => Ok(ok_mv(Value::Table(entry_table(&lua, &e)?))),
                        Err(e) => err_mv(&lua, error_key(&e)),
                    }
                }
            })?,
        )?;
    }

    // --- Mutations (engine Tasks: journal + policy + undo) ---------------
    for (key, mv) in [("copy", false), ("move", true)] {
        let backend = backend.clone();
        let cwd = ctx.cwd.clone();
        let cancellers = Rc::clone(&cancellers);
        let closed = Rc::clone(&closed);
        fs.set(
            key,
            lua.create_async_function(move |lua, (src, dst): (mlua::String, mlua::String)| {
                let backend = backend.clone();
                let cwd = cwd.clone();
                let cancellers = Rc::clone(&cancellers);
                let closed = Rc::clone(&closed);
                async move {
                    // Binding of a closed run (stash): dead, see rustdoc.
                    if closed.get() {
                        return err_mv(&lua, error_key(&Error::Unsupported));
                    }
                    let (from, to) = match (
                        to_vpath(&cwd, &src.as_bytes()),
                        to_vpath(&cwd, &dst.as_bytes()),
                    ) {
                        (Ok(f), Ok(t)) => (f, t),
                        (Err(key), _) | (_, Err(key)) => return err_mv(&lua, key),
                    };
                    let submitted = if mv {
                        backend.move_(&from, &to, TransferOptions::default()).await
                    } else {
                        backend.copy(&from, &to, TransferOptions::default()).await
                    };
                    match submitted {
                        Ok(task) => {
                            // BEFORE the join: if the user aborts the run,
                            // the driver can cancel this Task in flight. An
                            // ABANDONMENT with the remote submit still in
                            // flight is withdrawn by the backend's
                            // rpc.cancel guard (#74, see install_fs's
                            // rustdoc).
                            cancellers.borrow_mut().push(task.canceller());
                            finish(&lua, &task.join().await)
                        }
                        Err(e) => err_mv(&lua, error_key(&e)),
                    }
                }
            })?,
        )?;
    }
    {
        // Last use: `backend`, `cancellers` and `closed` are MOVED here.
        let cwd = ctx.cwd.clone();
        fs.set(
            "delete",
            lua.create_async_function(move |lua, path: mlua::String| {
                let backend = backend.clone();
                let cwd = cwd.clone();
                let cancellers = Rc::clone(&cancellers);
                let closed = Rc::clone(&closed);
                async move {
                    // Binding of a closed run (stash): dead, see rustdoc.
                    if closed.get() {
                        return err_mv(&lua, error_key(&Error::Unsupported));
                    }
                    let p = match to_vpath(&cwd, &path.as_bytes()) {
                        Ok(p) => p,
                        Err(key) => return err_mv(&lua, key),
                    };
                    // ALWAYS trash: permanent delete is NOT exposed in v1
                    // (spec M4 Lua) — a script does not delete irreversibly.
                    match backend.delete(&p, DeleteMode::Trash).await {
                        Ok(task) => {
                            // Same abandoned-submit guard as in
                            // copy/move (#74).
                            cancellers.borrow_mut().push(task.canceller());
                            finish(&lua, &task.join().await)
                        }
                        Err(e) => err_mv(&lua, error_key(&e)),
                    }
                }
            })?,
        )?;
    }
    norte.set("fs", fs)?;

    // --- norte.pane: frozen snapshot, synchronous functions ---------------
    let pane = lua.create_table()?;
    {
        let wire = ctx.cwd.to_wire();
        pane.set(
            "cwd",
            lua.create_function(move |lua, ()| lua.create_string(wire.as_bytes()))?,
        )?;
    }
    {
        let wire = ctx.other_cwd.to_wire();
        pane.set(
            "other_cwd",
            lua.create_function(move |lua, ()| lua.create_string(wire.as_bytes()))?,
        )?;
    }
    {
        let wires: Vec<String> = ctx.selection.iter().map(VPath::to_wire).collect();
        pane.set(
            "selection",
            lua.create_function(move |lua, ()| {
                let t = lua.create_table()?;
                for (i, w) in wires.iter().enumerate() {
                    t.set(i + 1, lua.create_string(w.as_bytes())?)?;
                }
                Ok(t)
            })?,
        )?;
    }
    {
        // Last use of `ctx`: `current` is consumed.
        let wire = ctx.current.map(|p| p.to_wire());
        pane.set(
            "current",
            lua.create_function(move |lua, ()| match &wire {
                Some(w) => Ok(Value::String(lua.create_string(w.as_bytes())?)),
                None => Ok(Value::Nil),
            })?,
        )?;
    }
    norte.set("pane", pane)?;

    // --- norte.ui.message: accumulates, the driver dumps it ---------------
    let ui: mlua::Table = norte.get("ui")?;
    ui.set(
        "message",
        lua.create_function(move |_, s: mlua::String| {
            let mut msgs = messages.borrow_mut();
            if msgs.len() < MESSAGES_MAX {
                msgs.push(String::from_utf8_lossy(&s.as_bytes()).into_owned());
            } else if let Some(last) = msgs.last_mut() {
                // Cap reached: discarded and the overflow is marked (see
                // MESSAGES_MAX).
                "…".clone_into(last);
            }
            Ok(())
        })?,
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vp(w: &str) -> VPath {
        VPath::parse(w).expect("wire")
    }

    /// UTF-8 absolute = wire interpretation (percent-decoding).
    #[test]
    fn absolute_wire_percent_decodes() {
        let base = vp("mem:///");
        let p = to_vpath(&base, b"mem:///%FF%FE").expect("wire");
        assert_eq!(p.file_name().unwrap().as_bytes(), &[0xFF, 0xFE]);
    }

    /// Non-UTF8 absolute (raw concatenation in Lua) = raw segments.
    #[test]
    fn absolute_raw_does_not_decode() {
        let base = vp("mem:///");
        let p = to_vpath(&base, b"mem:///\xFF\xFE").expect("raw");
        assert_eq!(p.file_name().unwrap().as_bytes(), &[0xFF, 0xFE]);
        assert_eq!(p.to_wire(), "mem:///%FF%FE");
    }

    /// Relative = raw, hung off the base, multi-segment included.
    #[test]
    fn relative_hangs_off_the_base_raw() {
        let base = vp("mem:///d");
        let p = to_vpath(&base, b"sub/100%").expect("relative");
        assert_eq!(p.to_wire(), "mem:///d/sub/100%25");
    }

    /// Invalid ones: empty, empty segment, `..`, NUL.
    #[test]
    fn invalid_ones_give_a_stable_key() {
        let base = vp("mem:///");
        for raw in [&b""[..], b"a//b", b"..", b"a\x00b", b"mem://\xFF/x"] {
            assert_eq!(
                to_vpath(&base, raw),
                Err(error_key(&Error::InvalidPath)),
                "{}",
                String::from_utf8_lossy(raw)
            );
        }
    }

    /// `norte.ui.message` has a cap: a looping script does not grow the
    /// queue without bound; the overflow is MARKED (last = "…").
    #[test]
    fn message_has_a_cap_and_marks_overflow() {
        let lua = Lua::new();
        let norte = lua.create_table().unwrap();
        norte.set("ui", lua.create_table().unwrap()).unwrap();
        lua.globals().set("norte", norte).unwrap();

        let backend = Backend::Embedded(std::sync::Arc::new(norte_core::Engine::new()));
        let ctx = PaneCtx {
            cwd: vp("mem:///"),
            other_cwd: vp("mem:///"),
            selection: vec![],
            current: None,
        };
        let messages: Rc<RefCell<Vec<String>>> = Rc::default();
        install_fs(
            &lua,
            backend,
            ctx,
            Rc::default(),
            Rc::clone(&messages),
            Rc::default(),
        )
        .unwrap();

        lua.load("for i = 1, 100 do norte.ui.message('m' .. i) end")
            .exec()
            .unwrap();
        let msgs = messages.borrow();
        assert_eq!(msgs.len(), MESSAGES_MAX, "never above the cap");
        assert_eq!(msgs.last().unwrap(), "…", "overflow marked");
        assert_eq!(msgs[0], "m1", "the first ones are kept");
    }
}
