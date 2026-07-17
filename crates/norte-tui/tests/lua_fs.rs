//! Bindings norte.fs sobre Engine+MemProvider real: byte strings, journal.

use std::sync::Arc;

use bytes::Bytes;
use norte_core::Engine;
use norte_core::backend::Backend;
use norte_proto::VPath;
use norte_testkit::MemProvider;
use norte_tui::lua::{LuaHost, PaneCtx};
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
async fn copy_list_stat_desde_lua() {
    let (backend, mem) = backend_mem();
    write_file(&mem, "mem:///a", b"datos").await;
    let h = LuaHost::new().expect("lua");
    let out = h
        .run_script_for_test(
            backend,
            ctx("mem:///"),
            br#"
                assert(norte.fs.copy("mem:///a", "mem:///b"))
                local e = assert(norte.fs.stat("mem:///b"))
                assert(e.size == 5, "size")
                local l = assert(norte.fs.list("mem:///"))
                return #l
            "#,
        )
        .await
        .expect("script ok");
    assert_eq!(out, 2);
}

#[tokio::test]
async fn bytes_no_utf8_round_trip() {
    let (backend, mem) = backend_mem();
    // Nombre con bytes 0xFF 0xFE: por el wire es percent-encoding.
    write_file(&mem, "mem:///%FF%FE", b"x").await;
    let h = LuaHost::new().expect("lua");
    // list devuelve el NOMBRE como byte string; copy con ese byte string
    // (concatenado en Lua) funciona — cero suposición UTF-8 (regla 1).
    let out = h
        .run_script_for_test(
            backend,
            ctx("mem:///"),
            br#"
                local l = assert(norte.fs.list("mem:///"))
                assert(#l == 1)
                local name = l[1].name
                assert(#name == 2, "dos bytes crudos")
                assert(norte.fs.copy("mem:///" .. name, "mem:///copia"))
                return true
            "#,
        )
        .await
        .expect("script ok");
    assert_eq!(out, 1); // true → 1 en la conversión del harness
}

#[tokio::test]
async fn error_del_protocolo_llega_como_nil_categoria() {
    let (backend, _mem) = backend_mem();
    let h = LuaHost::new().expect("lua");
    let out = h
        .run_script_for_test(
            backend,
            ctx("mem:///"),
            br#"
                local ok, err = norte.fs.stat("mem:///no-existe")
                assert(ok == nil and type(err) == "string")
                return true
            "#,
        )
        .await
        .expect("script ok");
    assert_eq!(out, 1);
}

#[tokio::test]
async fn pane_expone_el_snapshot() {
    let (backend, _mem) = backend_mem();
    let h = LuaHost::new().expect("lua");
    let mut c = ctx("mem:///");
    c.selection = vec![vp("mem:///a"), vp("mem:///b")];
    let out = h
        .run_script_for_test(backend, c, br"return #norte.pane.selection()")
        .await
        .expect("script ok");
    assert_eq!(out, 2);
}
