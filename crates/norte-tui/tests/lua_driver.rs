//! Driver: the command's future is polled inline; clean cancellation on two
//! fronts (engine Tasks + instruction hook) and a hard timeout.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use norte_core::Engine;
use norte_core::backend::Backend;
use norte_proto::VPath;
use norte_testkit::MemProvider;
use norte_tui::lua::{Layer, LuaHost, PaneCtx, RunOutcome};
use norte_vfs::Provider;

fn vp(w: &str) -> VPath {
    VPath::parse(w).expect("wire")
}

async fn write_file(mem: &MemProvider, wire: &str, content: &[u8]) {
    let mut sink = mem.write(&vp(wire)).await.unwrap();
    sink.write(Bytes::copy_from_slice(content)).await.unwrap();
    sink.commit().await.unwrap();
}

fn backend_mem() -> (Backend, Arc<MemProvider>) {
    let engine = Engine::new();
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    (Backend::Embedded(Arc::new(engine)), mem)
}

fn ctx(cwd: &str) -> PaneCtx {
    PaneCtx {
        cwd: vp(cwd),
        other_cwd: vp(cwd),
        selection: vec![],
        current: None,
    }
}

#[tokio::test]
async fn invoke_executes_and_reports_ok() {
    let (backend, mem) = backend_mem();
    write_file(&mem, "mem:///a", b"x").await;
    let h = LuaHost::new().expect("lua");
    h.eval_layer(
        b"norte.command('dup', function()\n\
            assert(norte.fs.copy('mem:///a', 'mem:///b'))\n\
            norte.ui.message('hola')\n\
          end)",
        Layer::User,
    )
    .expect("load");
    let token = tokio_util::sync::CancellationToken::new();
    let run = h
        .invoke("dup", backend.clone(), ctx("mem:///"), token)
        .expect("exists");
    let outcome = run.await;
    match outcome {
        RunOutcome::Ok { messages } => {
            assert_eq!(messages, vec!["hola".to_string()], "the run's messages");
        }
        other => panic!("expected Ok, got {other:?}"),
    }
    // Through Backend (not the provider directly) so as not to depend on its API.
    assert!(
        backend.stat(&vp("mem:///b")).await.is_ok(),
        "the copy happened"
    );
}

#[tokio::test]
async fn invoke_unknown_es_none() {
    let (backend, _mem) = backend_mem();
    let h = LuaHost::new().expect("lua");
    assert!(
        h.invoke(
            "nadie",
            backend,
            ctx("mem:///"),
            tokio_util::sync::CancellationToken::new()
        )
        .is_none()
    );
}

#[tokio::test]
async fn cancelling_kills_the_script_and_its_tasks() {
    let (backend, mem) = backend_mem();
    write_file(&mem, "mem:///a", b"x").await;
    // Front 1 for real: with per-operation latency, the copy Task is still
    // IN FLIGHT when the cancel arrives (with no latency it would complete
    // in microseconds and the canceller would be cancelling an already
    // terminal task).
    mem.faults()
        .set_latency_per_op(Some(Duration::from_millis(200)));
    let h = LuaHost::new().expect("lua");
    // The copy returns nil (cancelled) → the script falls into a pure Lua
    // loop (with no await points): the token must kill it via the
    // instruction hook, not just through await. A single run exercises
    // BOTH fronts.
    h.eval_layer(
        b"norte.command('loop', function()\n\
            local ok = norte.fs.copy('mem:///a', 'mem:///b')\n\
            while true do end\n\
          end)",
        Layer::User,
    )
    .expect("load");
    let token = tokio_util::sync::CancellationToken::new();
    let run = h
        .invoke("loop", backend.clone(), ctx("mem:///"), token.clone())
        .expect("exists");
    let cancel = async {
        tokio::time::sleep(Duration::from_millis(100)).await;
        token.cancel();
    };
    let (outcome, ()) = tokio::join!(run, cancel);
    assert!(matches!(outcome, RunOutcome::Cancelled), "{outcome:?}");

    // Front 1 verified: the copy Task died cancelled BEFORE completing —
    // the destination does not exist (with ~200ms/op and a cancel at
    // 100ms, the commit could not have happened; if the canceller did not
    // work, the copy would complete and `mem:///b` would exist).
    mem.faults().clear();
    assert!(
        backend.stat(&vp("mem:///b")).await.is_err(),
        "the in-flight Task should have died cancelled, not completed"
    );

    // The instruction hook must NOT stay set: the Lua state is shared and a
    // later run has to work normally.
    let out = h
        .run_script_for_test(backend, ctx("mem:///"), b"return 7")
        .await
        .expect("the lua state was left clean after cancelling");
    assert_eq!(out, 7);
}

#[tokio::test]
async fn timeout_duro_abandona() {
    // Pure Lua loop with NO cancel and a short injected timeout: the driver
    // ABANDONS the run's future (drop) and reports TimedOut.
    let (backend, _mem) = backend_mem();
    let h = LuaHost::new().expect("lua");
    h.eval_layer(
        b"norte.command('loop', function() while true do end end)",
        Layer::User,
    )
    .expect("load");
    let run = h
        .invoke_with_timeout(
            "loop",
            backend.clone(),
            ctx("mem:///"),
            tokio_util::sync::CancellationToken::new(),
            Duration::from_millis(200),
        )
        .expect("exists");
    assert!(matches!(run.await, RunOutcome::TimedOut));

    // The guard lives INSIDE the abandoned future: its Drop runs when the
    // future is dropped and must remove the hook — another trivial run works.
    let out = h
        .run_script_for_test(backend, ctx("mem:///"), b"return 3")
        .await
        .expect("the lua state was left clean after abandoning");
    assert_eq!(out, 3);
}

#[tokio::test]
async fn run_closed_kills_stashed_bindings() {
    let (backend, mem) = backend_mem();
    write_file(&mem, "mem:///a", b"x").await;
    let h = LuaHost::new().expect("lua");
    // Run 1 stashes an fs binding in a global; run 2 invokes it: run 1's
    // binding (closed) must return nil+err, not operate with the old ctx.
    h.eval_layer(
        b"norte.command('stashea', function() stash = norte.fs.stat end)\n\
          norte.command('usa', function()\n\
            local ok, err = stash('mem:///a')\n\
            assert(ok == nil, 'the closed run binding must fail')\n\
            assert(type(err) == 'string')\n\
          end)",
        Layer::User,
    )
    .expect("load");

    let run1 = h
        .invoke(
            "stashea",
            backend.clone(),
            ctx("mem:///"),
            tokio_util::sync::CancellationToken::new(),
        )
        .expect("exists");
    assert!(matches!(run1.await, RunOutcome::Ok { .. }));

    let run2 = h
        .invoke(
            "usa",
            backend,
            ctx("mem:///"),
            tokio_util::sync::CancellationToken::new(),
        )
        .expect("exists");
    let outcome = run2.await;
    assert!(
        matches!(outcome, RunOutcome::Ok { .. }),
        "the script's asserts pass: {outcome:?}"
    );
}
