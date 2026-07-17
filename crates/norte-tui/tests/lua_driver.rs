//! Driver: el future del comando se pollea inline; cancelación limpia en dos
//! frentes (Tasks del engine + hook de instrucciones) y timeout duro.

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
async fn invoke_ejecuta_y_reporta_ok() {
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
    .expect("carga");
    let token = tokio_util::sync::CancellationToken::new();
    let run = h
        .invoke("dup", backend.clone(), ctx("mem:///"), token)
        .expect("existe");
    let outcome = run.await;
    match outcome {
        RunOutcome::Ok { messages } => {
            assert_eq!(messages, vec!["hola".to_string()], "mensajes del run");
        }
        other => panic!("esperaba Ok, fue {other:?}"),
    }
    // Via Backend (no el provider directo) para no depender de su API.
    assert!(
        backend.stat(&vp("mem:///b")).await.is_ok(),
        "el copy ocurrió"
    );
}

#[tokio::test]
async fn invoke_desconocido_es_none() {
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
async fn cancelar_mata_el_script_y_sus_tasks() {
    let (backend, mem) = backend_mem();
    write_file(&mem, "mem:///a", b"x").await;
    let h = LuaHost::new().expect("lua");
    // Script que copia y luego SE QUEDA en bucle Lua puro (sin puntos await):
    // el token debe matarlo vía hook de instrucciones, no solo por await.
    h.eval_layer(
        b"norte.command('loop', function()\n\
            assert(norte.fs.copy('mem:///a', 'mem:///b'))\n\
            while true do end\n\
          end)",
        Layer::User,
    )
    .expect("carga");
    let token = tokio_util::sync::CancellationToken::new();
    let run = h
        .invoke("loop", backend.clone(), ctx("mem:///"), token.clone())
        .expect("existe");
    let cancel = async {
        tokio::time::sleep(Duration::from_millis(100)).await;
        token.cancel();
    };
    let (outcome, ()) = tokio::join!(run, cancel);
    assert!(matches!(outcome, RunOutcome::Cancelled), "{outcome:?}");

    // El hook de instrucciones NO puede quedar puesto: el estado Lua es
    // compartido y un run posterior debe funcionar con normalidad.
    let out = h
        .run_script_for_test(backend, ctx("mem:///"), b"return 7")
        .await
        .expect("el estado lua quedó limpio tras cancelar");
    assert_eq!(out, 7);
}

#[tokio::test]
async fn timeout_duro_abandona() {
    // Bucle Lua puro SIN cancelar y con timeout corto inyectado: el driver
    // ABANDONA el future del run (drop) y reporta TimedOut.
    let (backend, _mem) = backend_mem();
    let h = LuaHost::new().expect("lua");
    h.eval_layer(
        b"norte.command('loop', function() while true do end end)",
        Layer::User,
    )
    .expect("carga");
    let run = h
        .invoke_with_timeout(
            "loop",
            backend.clone(),
            ctx("mem:///"),
            tokio_util::sync::CancellationToken::new(),
            Duration::from_millis(200),
        )
        .expect("existe");
    assert!(matches!(run.await, RunOutcome::TimedOut));

    // El guard vive DENTRO del future abandonado: su Drop corre al dropear
    // el future y debe quitar el hook — otro run trivial funciona.
    let out = h
        .run_script_for_test(backend, ctx("mem:///"), b"return 3")
        .await
        .expect("el estado lua quedó limpio tras abandonar");
    assert_eq!(out, 3);
}

#[tokio::test]
async fn run_cerrado_mata_bindings_stasheados() {
    let (backend, mem) = backend_mem();
    write_file(&mem, "mem:///a", b"x").await;
    let h = LuaHost::new().expect("lua");
    // Run 1 guarda un binding fs en un global; run 2 lo invoca: el binding
    // del run 1 (cerrado) debe devolver nil+err, no operar con el ctx viejo.
    h.eval_layer(
        b"norte.command('stashea', function() stash = norte.fs.stat end)\n\
          norte.command('usa', function()\n\
            local ok, err = stash('mem:///a')\n\
            assert(ok == nil, 'el binding del run cerrado debe fallar')\n\
            assert(type(err) == 'string')\n\
          end)",
        Layer::User,
    )
    .expect("carga");

    let run1 = h
        .invoke(
            "stashea",
            backend.clone(),
            ctx("mem:///"),
            tokio_util::sync::CancellationToken::new(),
        )
        .expect("existe");
    assert!(matches!(run1.await, RunOutcome::Ok { .. }));

    let run2 = h
        .invoke(
            "usa",
            backend,
            ctx("mem:///"),
            tokio_util::sync::CancellationToken::new(),
        )
        .expect("existe");
    let outcome = run2.await;
    assert!(
        matches!(outcome, RunOutcome::Ok { .. }),
        "los asserts del script pasan: {outcome:?}"
    );
}
