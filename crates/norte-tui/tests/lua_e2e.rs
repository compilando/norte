//! E2E del criterio de salida de la spec M4 Lua
//! (`docs/superpowers/specs/2026-07-17-m4-lua-scripting-design.md`): un
//! `init.lua` REALISTA registra un comando que copia la selección al otro
//! pane renombrando (`copia-<basename>`), todo vía `Backend` → engine
//! (journal + policy + undo); sin terminal (patrón `backend_mem` de
//! `lua_fs.rs`). Cubre: camino feliz byte-exacto, nombre hostil (bytes
//! crudos `0xFF`), cancelación limpia por la ruta E2E completa y rastro
//! deshacible en el journal.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use norte_core::backend::Backend;
use norte_core::{Actor, Engine, Journal, SqliteJournal};
use norte_proto::VPath;
use norte_testkit::MemProvider;
use norte_tui::lua::{Layer, LuaHost, PaneCtx, RunOutcome};
use norte_vfs::Provider;

/// El `init.lua` realista del criterio de salida. `basename` en Lua PURO
/// sobre byte strings: búsqueda del último `/` byte a byte (`string.byte`),
/// SIN patrones Lua (`string.match`/`find` con patrón tratarían `%` y bytes
/// no-UTF8 como sintaxis — aquí solo aritmética de bytes, robusto ante
/// cualquier nombre). Los paths de `norte.pane.*` llegan en forma wire
/// (UTF-8; bytes no-UTF8, controles y `%` van percent-escapados), así que
/// la concatenación absoluta re-parsea como wire — round-trip exacto
/// también para el nombre hostil `0xFF`.
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

/// Engine embebido + `MemProvider` con el directorio destino `mem:///dst`
/// ya creado (el `MemProvider` exige que el padre exista — `mkdir` va por
/// el provider directo porque `Backend` no lo expone, ver la desviación
/// documentada en `lua/fs.rs`).
async fn backend_mem() -> (Backend, Arc<MemProvider>) {
    let engine = Engine::new();
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    mem.mkdir(&vp("mem:///dst")).await.expect("mkdir dst");
    (Backend::Embedded(Arc::new(engine)), mem)
}

/// Host con el `init.lua` realista ya cargado como capa de usuario.
fn host_con_init() -> LuaHost {
    let h = LuaHost::new().expect("lua");
    let w = h.eval_layer(INIT_LUA, Layer::User).expect("init.lua carga");
    assert!(w.is_empty(), "sin warnings de carga: {w:?}");
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

async fn invoke_copiar_sel(h: &LuaHost, backend: &Backend, selection: Vec<VPath>) -> RunOutcome {
    let run = h
        .invoke(
            "copiar-sel",
            backend.clone(),
            ctx(selection),
            tokio_util::sync::CancellationToken::new(),
        )
        .expect("copiar-sel registrado");
    run.await
}

/// Criterio de salida, camino feliz: la selección entera acaba en el otro
/// pane renombrada y BYTE-EXACTA — todo por el engine.
#[tokio::test]
async fn copia_la_seleccion_al_otro_pane_renombrada() {
    let (backend, mem) = backend_mem().await;
    write_file(&mem, "mem:///a", b"contenido de a").await;
    write_file(&mem, "mem:///b", b"be").await;
    let h = host_con_init();

    let outcome = invoke_copiar_sel(&h, &backend, vec![vp("mem:///a"), vp("mem:///b")]).await;
    match outcome {
        RunOutcome::Ok { messages } => {
            assert_eq!(messages, vec!["copiado".to_string()]);
        }
        other => panic!("esperaba Ok, fue {other:?}"),
    }

    // Asserts vía Backend (la misma superficie que usa el script).
    for (wire, contenido) in [
        ("mem:///dst/copia-a", &b"contenido de a"[..]),
        ("mem:///dst/copia-b", &b"be"[..]),
    ] {
        assert!(backend.stat(&vp(wire)).await.is_ok(), "{wire} existe");
        let bytes = backend.read(&vp(wire), None).await.expect("read");
        assert_eq!(bytes, contenido, "{wire} byte-exacto");
    }
}

/// Nombre hostil: bytes crudos `0xFF` (no-UTF8). `selection()` lo entrega
/// en forma wire (`mem:///%FF`), el basename Lua opera sobre esos bytes
/// ASCII y la concatenación re-parsea como wire → el destino es
/// `copia-<0xFF>` con el byte CRUDO, verificado con el mismo `VPath` que
/// en `lua_fs.rs` (regla 1: cero suposición UTF-8 en todo el camino).
#[tokio::test]
async fn nombre_hostil_bytes_crudos_round_trip() {
    let (backend, mem) = backend_mem().await;
    write_file(&mem, "mem:///%FF", b"hostil").await;
    let h = host_con_init();

    let outcome = invoke_copiar_sel(&h, &backend, vec![vp("mem:///%FF")]).await;
    assert!(matches!(outcome, RunOutcome::Ok { .. }), "{outcome:?}");

    let dst = vp("mem:///dst/copia-%FF");
    assert_eq!(
        dst.file_name().unwrap().as_bytes(),
        b"copia-\xFF",
        "el segmento destino lleva el byte crudo"
    );
    assert!(backend.stat(&dst).await.is_ok(), "copia-<0xFF> existe");
    assert_eq!(backend.read(&dst, None).await.expect("read"), b"hostil");
}

/// Cancelación limpia por la ruta E2E completa: el MISMO `init.lua`
/// realista, con latencia por operación para que la Task de copia siga en
/// vuelo cuando llega el Esc-equivalente (`token.cancel`). El run muere
/// `Cancelled` y el destino NO aparece (mismo patrón que `lua_driver.rs`,
/// pero atravesando el comando real del usuario).
/// `start_paused`: latencia inyectada y cancel usan timers tokio — con el
/// reloj pausado el runtime avanza el tiempo al quedar ocioso (determinista
/// e instantáneo, sin carreras de wall-clock).
#[tokio::test(start_paused = true)]
async fn esc_cancela_limpio_sin_destino_a_medias() {
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
        .expect("copiar-sel registrado");
    let cancel = async {
        tokio::time::sleep(Duration::from_millis(100)).await;
        token.cancel();
    };
    let (outcome, ()) = tokio::join!(run, cancel);
    assert!(matches!(outcome, RunOutcome::Cancelled), "{outcome:?}");

    mem.faults().clear();
    assert!(
        backend.stat(&vp("mem:///dst/copia-a")).await.is_err(),
        "la Task en vuelo murió cancelada: el destino no aparece"
    );
}

/// Undo-ability: el copy del comando deja rastro DESHACIBLE en el journal.
/// Montarlo aquí es barato (`Journal::open_in_memory` + `with_journal`,
/// mismo patrón que `norte-core/tests/engine_journal.rs`), así que se
/// verifica de verdad: entrada registrada Y revertible para el actor User
/// (la mecánica completa del undo la cubren los tests de engine — todo va
/// por `Backend`, no hay camino aparte que probar aquí).
#[tokio::test]
async fn el_copy_del_comando_deja_rastro_deshacible_en_el_journal() {
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

    let outcome = invoke_copiar_sel(&h, &backend, vec![vp("mem:///a")]).await;
    assert!(matches!(outcome, RunOutcome::Ok { .. }), "{outcome:?}");

    let entries = journal.journal().entries().await.expect("entries");
    assert!(!entries.is_empty(), "el copy quedó en el journal");
    let revertibles = journal
        .journal()
        .revertible_for(&Actor::User)
        .await
        .expect("revertible_for");
    // Endurecido (rust review): no basta "no vacío" — ALGUNA entrada
    // revertible es EXACTAMENTE la de este copy: actor User y el path del
    // DESTINO creado (`dst/copia-a`, forma wire).
    assert!(
        revertibles.iter().any(|e| {
            e.actor_kind == "user"
                && e.op == "created"
                && e.path == vp("mem:///dst/copia-a").to_wire().into_bytes()
        }),
        "la entrada revertible del copy referencia el destino: {revertibles:?}"
    );
}
