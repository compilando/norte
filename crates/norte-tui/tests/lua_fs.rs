//! norte.fs bindings over a real Engine+MemProvider: byte strings, journal.

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
async fn copy_list_stat_from_lua() {
    let (backend, mem) = backend_mem();
    write_file(&mem, "mem:///a", b"data").await;
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
async fn non_utf8_bytes_round_trip() {
    let (backend, mem) = backend_mem();
    // Name with bytes 0xFF 0xFE: over the wire it is percent-encoding.
    write_file(&mem, "mem:///%FF%FE", b"x").await;
    let h = LuaHost::new().expect("lua");
    // list returns the NAME as a byte string; copy with that byte string
    // (concatenated in Lua) works — zero UTF-8 assumption (rule 1).
    let out = h
        .run_script_for_test(
            backend,
            ctx("mem:///"),
            br#"
                local l = assert(norte.fs.list("mem:///"))
                assert(#l == 1)
                local name = l[1].name
                assert(#name == 2, "two raw bytes")
                assert(norte.fs.copy("mem:///" .. name, "mem:///copy"))
                return true
            "#,
        )
        .await
        .expect("script ok");
    assert_eq!(out, 1); // true → 1 in the harness's conversion
}

#[tokio::test]
async fn protocol_error_arrives_as_nil_category() {
    let (backend, _mem) = backend_mem();
    let h = LuaHost::new().expect("lua");
    let out = h
        .run_script_for_test(
            backend,
            ctx("mem:///"),
            br#"
                local ok, err = norte.fs.stat("mem:///does-not-exist")
                assert(ok == nil and type(err) == "string")
                return true
            "#,
        )
        .await
        .expect("script ok");
    assert_eq!(out, 1);
}

#[tokio::test]
async fn pane_exposes_the_snapshot() {
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

/// Writes `content` under the root with a RAW-bytes name (corpus).
async fn write_named(mem: &MemProvider, name: &[u8], content: &[u8]) -> VPath {
    let p = MemProvider::root().join(norte_proto::Segment::new(name.to_vec()).expect("segment"));
    let mut sink = mem.write(&p).await.unwrap();
    sink.write(Bytes::copy_from_slice(content)).await.unwrap();
    sink.commit().await.unwrap();
    p
}

/// Encoding review M4 Lua: the WHOLE hostile corpus round-trips via the
/// RECOMMENDED PATH for absolutes (`entry.path`, the complete wire):
/// `list` → `stat(l[1].path)` OK and `name` byte-exact against the fixture.
#[tokio::test]
async fn hostile_corpus_round_trips_via_entry_path() {
    for n in norte_testkit::corpus::hostile_names() {
        let (backend, mem) = backend_mem();
        write_named(&mem, &n.bytes, b"x").await;
        let h = LuaHost::new().expect("lua");
        // The expected bytes enter Lua as a literal with DECIMAL escapes
        // (`"\97\37…"`): byte-exact with no UTF-8 assumption and without
        // `string.char(...)`'s argument cap (name_max_255 blows it with
        // 255 arguments).
        let script = format!(
            "local expected = \"{bytes}\"\n\
             local l = assert(norte.fs.list('mem:///'))\n\
             assert(#l == 1, 'one entry')\n\
             assert(l[1].name == expected, 'byte-exact name')\n\
             local e = assert(norte.fs.stat(l[1].path))\n\
             assert(e.name == expected, 'stat via entry.path round-trip')\n\
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

/// Pin of the ACCEPTED risk (spec M4 Lua, Deviations point 8) with the
/// `percent_lookalike_download` fixture (`a%20b.txt`, normal downloads-style
/// UTF-8): concatenated as ABSOLUTE the `%20` gets decoded and does NOT
/// reach the original file; the recommended path (`entry.path`) and the
/// RELATIVE one with the raw `name` do reach it. VISIBLE contract of the
/// ambiguity.
#[tokio::test]
async fn percent_ambiguity_in_absolutes_pinned_by_the_corpus() {
    let n = norte_testkit::corpus::hostile_names()
        .into_iter()
        .find(|n| n.id == "percent_lookalike_download")
        .expect("corpus fixture");
    let (backend, mem) = backend_mem();
    write_named(&mem, &n.bytes, b"original").await;
    let h = LuaHost::new().expect("lua");
    let out = h
        .run_script_for_test(
            backend,
            ctx("mem:///"),
            br"
                local l = assert(norte.fs.list('mem:///'))
                local name = l[1].name -- 'a%20b.txt' (raw bytes)
                -- Concatenated ABSOLUTE: decodes %20 -> 'a b.txt' -> does
                -- NOT exist. Accepted and documented risk (to_vpath).
                local ok = norte.fs.stat('mem:///' .. name)
                assert(ok == nil, 'the concatenated absolute does not reach the original')
                -- Recommended path: the complete wire round-trips.
                assert(norte.fs.stat(l[1].path), 'entry.path reaches it')
                -- And the RELATIVE one with the raw name does not decode: it reaches it.
                assert(norte.fs.stat(name), 'the raw relative reaches it')
                return true
            ",
        )
        .await
        .expect("script ok");
    assert_eq!(out, 1);
}
