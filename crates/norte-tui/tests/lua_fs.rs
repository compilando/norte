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

/// Escribe `content` bajo la raíz con un nombre de BYTES crudos (corpus).
async fn write_named(mem: &MemProvider, name: &[u8], content: &[u8]) -> VPath {
    let p = MemProvider::root().join(norte_proto::Segment::new(name.to_vec()).expect("segmento"));
    let mut sink = mem.write(&p).await.unwrap();
    sink.write(Bytes::copy_from_slice(content)).await.unwrap();
    sink.commit().await.unwrap();
    p
}

/// Encoding review M4 Lua: TODO el corpus hostil hace round-trip por la
/// RUTA RECOMENDADA para absolutos (`entry.path`, el wire completo):
/// `list` → `stat(l[1].path)` OK y `name` byte-exacto contra la fixture.
#[tokio::test]
async fn corpus_hostil_round_trip_por_entry_path() {
    for n in norte_testkit::corpus::hostile_names() {
        let (backend, mem) = backend_mem();
        write_named(&mem, &n.bytes, b"x").await;
        let h = LuaHost::new().expect("lua");
        // Los bytes esperados entran a Lua como literal con escapes
        // DECIMALES (`"\97\37…"`): byte-exacto sin suposición UTF-8 y sin
        // el tope de registros de `string.char(...)` (name_max_255 lo
        // revienta con 255 argumentos).
        let script = format!(
            "local expected = \"{bytes}\"\n\
             local l = assert(norte.fs.list('mem:///'))\n\
             assert(#l == 1, 'una entrada')\n\
             assert(l[1].name == expected, 'name byte-exacto')\n\
             local e = assert(norte.fs.stat(l[1].path))\n\
             assert(e.name == expected, 'stat por entry.path round-trip')\n\
             return true",
            bytes = n.bytes.iter().fold(String::new(), |mut s, b| {
                use std::fmt::Write as _;
                let _ = write!(s, "\\{b}");
                s
            })
        );
        let out = h
            .run_script_for_test(backend, ctx("mem:///"), script.as_bytes())
            .await
            .unwrap_or_else(|e| panic!("[{}] script: {e}", n.id));
        assert_eq!(out, 1, "[{}]", n.id);
    }
}

/// Pin del riesgo ACEPTADO (spec M4 Lua, Desviaciones punto 8) con la
/// fixture `percent_lookalike_download` (`a%20b.txt`, UTF-8 normal estilo
/// descargas): concatenado como ABSOLUTO el `%20` se decodifica y NO llega
/// al fichero original; la ruta recomendada (`entry.path`) y el RELATIVO
/// con el `name` crudo sí llegan. Contrato VISIBLE de la ambigüedad.
#[tokio::test]
async fn ambiguedad_percent_en_absolutos_pinneada_con_el_corpus() {
    let n = norte_testkit::corpus::hostile_names()
        .into_iter()
        .find(|n| n.id == "percent_lookalike_download")
        .expect("fixture del corpus");
    let (backend, mem) = backend_mem();
    write_named(&mem, &n.bytes, b"original").await;
    let h = LuaHost::new().expect("lua");
    let out = h
        .run_script_for_test(
            backend,
            ctx("mem:///"),
            br"
                local l = assert(norte.fs.list('mem:///'))
                local name = l[1].name -- 'a%20b.txt' (bytes crudos)
                -- ABSOLUTO concatenado: decodifica %20 -> 'a b.txt' -> NO
                -- existe. Riesgo aceptado y documentado (to_vpath).
                local ok = norte.fs.stat('mem:///' .. name)
                assert(ok == nil, 'el absoluto concatenado no llega al original')
                -- Ruta recomendada: el wire completo hace round-trip.
                assert(norte.fs.stat(l[1].path), 'entry.path llega')
                -- Y el RELATIVO con el name crudo no decodifica: llega.
                assert(norte.fs.stat(name), 'el relativo crudo llega')
                return true
            ",
        )
        .await
        .expect("script ok");
    assert_eq!(out, 1);
}
