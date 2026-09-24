//! E2E for the M4 Lua acceptance criterion described in ADR 0026: a
//! REALISTIC `init.lua` registers a command that copies the selection to
//! the other pane while renaming (`copy-<basename>`), all through
//! `Backend` → engine (journal + policy + undo); no terminal (`lua_fs.rs`'s
//! `backend_mem` pattern). Covers: the byte-exact happy path, a hostile
//! name (raw `0xFF` bytes), clean cancellation through the full E2E route,
//! and an undoable trail in the journal.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use norte_core::backend::Backend;
use norte_core::{Actor, Engine, Journal, SqliteJournal};
use norte_proto::VPath;
use norte_testkit::MemProvider;
use norte_tui::lua::{Layer, LuaHost, PaneCtx, RunOutcome};
use norte_vfs::Provider;

/// The acceptance criterion's realistic `init.lua`. `basename` in PURE Lua
/// over byte strings: searches for the last `/` byte by byte
/// (`string.byte`), with NO Lua patterns (`string.match`/`find` with a
/// pattern would treat `%` and non-UTF8 bytes as syntax — here it is only
/// byte arithmetic, robust against any name). `norte.pane.*`'s paths
/// arrive in wire form (UTF-8; non-UTF8 bytes, controls and `%` come
/// percent-escaped), so the absolute concatenation re-parses as wire — an
/// exact round trip for the hostile `0xFF` name too.
const INIT_LUA: &[u8] = br"
    local function basename(p)
      local i = #p
      while i > 0 do
        if string.byte(p, i) == 47 then break end -- 47 = '/'
        i = i - 1
      end
      return string.sub(p, i + 1)
    end

    norte.command('copiar-sel', function()
      for _, p in ipairs(norte.pane.selection()) do
        local dst = norte.pane.other_cwd() .. '/copia-' .. basename(p)
        assert(norte.fs.copy(p, dst))
      end
      norte.ui.message('copiado')
    end)
";

fn vp(w: &str) -> VPath {
    VPath::parse(w).expect("wire")
}

async fn write_file(mem: &MemProvider, wire: &str, content: &[u8]) {
    let mut sink = mem.write(&vp(wire)).await.unwrap();
    sink.write(Bytes::copy_from_slice(content)).await.unwrap();
    sink.commit().await.unwrap();
}

/// Embedded engine + `MemProvider` with the destination directory
/// `mem:///dst` already created (`MemProvider` requires the parent to
/// exist — `mkdir` goes through the provider directly because `Backend`
/// does not expose it, see the deviation documented in `lua/fs.rs`).
async fn backend_mem() -> (Backend, Arc<MemProvider>) {
    let engine = Engine::new();
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    mem.mkdir(&vp("mem:///dst")).await.expect("mkdir dst");
    (Backend::Embedded(Arc::new(engine)), mem)
}

/// A host with the realistic `init.lua` already loaded as a user layer.
fn host_con_init() -> LuaHost {
    let h = LuaHost::new().expect("lua");
    let w = h.eval_layer(INIT_LUA, Layer::User).expect("init.lua loads");
    assert!(w.is_empty(), "no load warnings: {w:?}");
    h
}

fn ctx(selection: Vec<VPath>) -> PaneCtx {
    PaneCtx {
        cwd: vp("mem:///"),
        other_cwd: vp("mem:///dst"),
        selection,
        current: None,
    }
}

async fn invoke_copy_sel(h: &LuaHost, backend: &Backend, selection: Vec<VPath>) -> RunOutcome {
    let run = h
        .invoke(
            "copiar-sel",
            backend.clone(),
            ctx(selection),
            tokio_util::sync::CancellationToken::new(),
        )
        .expect("copiar-sel registered");
    run.await
}

/// Acceptance criterion, happy path: the whole selection ends up in the
/// other pane, renamed and BYTE-EXACT — all through the engine.
#[tokio::test]
async fn copies_the_selection_to_the_other_pane_renamed() {
    let (backend, mem) = backend_mem().await;
    write_file(&mem, "mem:///a", b"contenido de a").await;
    write_file(&mem, "mem:///b", b"be").await;
    let h = host_con_init();

    let outcome = invoke_copy_sel(&h, &backend, vec![vp("mem:///a"), vp("mem:///b")]).await;
    match outcome {
        RunOutcome::Ok { messages } => {
            assert_eq!(messages, vec!["copiado".to_string()]);
        }
        other => panic!("expected Ok, got {other:?}"),
    }

    // Asserts through Backend (the same surface the script uses).
    for (wire, content) in [
        ("mem:///dst/copia-a", &b"contenido de a"[..]),
        ("mem:///dst/copia-b", &b"be"[..]),
    ] {
        assert!(backend.stat(&vp(wire)).await.is_ok(), "{wire} exists");
        let bytes = backend.read(&vp(wire), None).await.expect("read");
        assert_eq!(bytes, content, "{wire} byte-exact");
    }
}

/// Hostile name: raw `0xFF` bytes (non-UTF8). `selection()` delivers it in
/// wire form (`mem:///%FF`), the Lua basename operates over those ASCII
/// bytes and the concatenation re-parses as wire → the destination is
/// `copy-<0xFF>` with the RAW byte, checked with the same `VPath` as in
/// `lua_fs.rs` (rule 1: zero UTF-8 assumption along the whole path).
#[tokio::test]
async fn hostile_name_raw_bytes_round_trip() {
    let (backend, mem) = backend_mem().await;
    write_file(&mem, "mem:///%FF", b"hostil").await;
    let h = host_con_init();

    let outcome = invoke_copy_sel(&h, &backend, vec![vp("mem:///%FF")]).await;
    assert!(matches!(outcome, RunOutcome::Ok { .. }), "{outcome:?}");

    let dst = vp("mem:///dst/copia-%FF");
    assert_eq!(
        dst.file_name().unwrap().as_bytes(),
        b"copia-\xFF",
        "the destination segment carries the raw byte"
    );
    assert!(backend.stat(&dst).await.is_ok(), "copia-<0xFF> exists");
    assert_eq!(backend.read(&dst, None).await.expect("read"), b"hostil");
}

/// Clean cancellation through the full E2E route: the SAME realistic
/// `init.lua`, with per-operation latency so the copy Task stays in flight
/// when the Esc-equivalent (`token.cancel`) arrives. The run dies
/// `Cancelled` and the destination does NOT appear (same pattern as
/// `lua_driver.rs`, but going through the user's real command).
/// `start_paused`: the injected latency and the cancel use tokio timers —
/// with the clock paused the runtime advances time when idle (deterministic
/// and instant, with no wall-clock races).
#[tokio::test(start_paused = true)]
async fn esc_cancels_cleanly_without_a_half_finished_destination() {
    let (backend, mem) = backend_mem().await;
    write_file(&mem, "mem:///a", b"contenido de a").await;
    mem.faults()
        .set_latency_per_op(Some(Duration::from_millis(200)));
    let h = host_con_init();

    let token = tokio_util::sync::CancellationToken::new();
    let run = h
        .invoke(
            "copiar-sel",
            backend.clone(),
            ctx(vec![vp("mem:///a")]),
            token.clone(),
        )
        .expect("copiar-sel registered");
    let cancel = async {
        tokio::time::sleep(Duration::from_millis(100)).await;
        token.cancel();
    };
    let (outcome, ()) = tokio::join!(run, cancel);
    assert!(matches!(outcome, RunOutcome::Cancelled), "{outcome:?}");

    mem.faults().clear();
    assert!(
        backend.stat(&vp("mem:///dst/copia-a")).await.is_err(),
        "the in-flight Task died cancelled: the destination does not appear"
    );
}

/// Undo-ability: the command's copy leaves an UNDOABLE trail in the
/// journal. Setting this up here is cheap (`Journal::open_in_memory` +
/// `with_journal`, the same pattern as
/// `norte-core/tests/engine_journal.rs`), so it is really checked: an entry
/// is recorded AND revertible for the User actor (the engine tests cover
/// undo's full mechanics — everything goes through `Backend`, there is no
/// separate path to test here).
#[tokio::test]
async fn the_command_copy_leaves_an_undoable_trail_in_the_journal() {
    let journal = Arc::new(SqliteJournal::new(
        Journal::open_in_memory().await.expect("journal open"),
    ));
    let engine = Engine::with_journal(Arc::clone(&journal));
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    mem.mkdir(&vp("mem:///dst")).await.expect("mkdir dst");
    let backend = Backend::Embedded(Arc::new(engine));
    write_file(&mem, "mem:///a", b"contenido de a").await;
    let h = host_con_init();

    let outcome = invoke_copy_sel(&h, &backend, vec![vp("mem:///a")]).await;
    assert!(matches!(outcome, RunOutcome::Ok { .. }), "{outcome:?}");

    let entries = journal.journal().entries().await.expect("entries");
    assert!(!entries.is_empty(), "the copy was left in the journal");
    let revertibles = journal
        .journal()
        .revertible_for(&Actor::User)
        .await
        .expect("revertible_for");
    // Hardened (rust review): "not empty" is not enough — SOME revertible
    // entry is EXACTLY this copy's: User actor and the path of the
    // DESTINATION created (`dst/copy-a`, wire form).
    assert!(
        revertibles.iter().any(|e| {
            e.actor_kind == "user"
                && e.op == "created"
                && e.path == vp("mem:///dst/copia-a").to_wire().into_bytes()
        }),
        "the copy's revertible entry references the destination: {revertibles:?}"
    );
}
