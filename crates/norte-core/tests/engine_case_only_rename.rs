//! #274: renombrar cambiando SOLO la caja (o la normalización) en un volumen
//! que pliega es trabajo de verdad, no una operación imposible.
//!
//! El planificador de lotes ya lo trata así
//! (`a_case_only_rename_is_real_work_on_a_case_insensitive_directory`), y la
//! issue decía que el renombrado de UNO contestaba otra cosa.
//!
//! **Lo que estos tests establecen es que por el camino que la issue nombra no
//! pasa.** `ops::move_task` sobre el MISMO provider va a `rename_with_policy`
//! y no consulta `same_node` en ningún punto — las dos únicas llamadas a
//! `same_node` están en `copy_task` y en `move_by_copy`, y a la segunda solo
//! se llega cuando el rename devuelve `Unsupported` (EXDEV entre montajes).
//! Así que un `Foo.txt → foo.txt` dentro del mismo directorio se ejecuta.
//!
//! Queda vivo el caso ESTRECHO que sí lo tocaría: un move que degrada a
//! copy+delete por EXDEV sobre un volumen que pliega. Ése no se puede montar
//! aquí —pide dos montajes reales, uno de ellos plegando— y por eso no tiene
//! test: reproducirlo es lo que #214 documenta que cuesta.

use std::sync::Arc;

use bytes::Bytes;
use norte_core::Engine;
use norte_proto::{CapabilityFlags, TaskState, VPath};
use norte_testkit::MemProvider;
use norte_vfs::Provider;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("wire válido")
}

async fn escribe(mem: &MemProvider, wire: &str) {
    let mut sink = mem.write(&vp(wire)).await.expect("write");
    sink.write(Bytes::from_static(b"x")).await.expect("chunk");
    sink.commit().await.expect("commit");
}

/// Un provider que NO distingue la caja, como APFS, NTFS o exFAT.
async fn engine_que_pliega() -> Engine {
    let mem = Arc::new(MemProvider::with_flags(
        CapabilityFlags::RENAME_ATOMIC | CapabilityFlags::CASE_PRESERVING,
    ));
    mem.mkdir(&vp("mem:///casa")).await.expect("casa");
    escribe(&mem, "mem:///casa/Foo.txt").await;
    let engine = Engine::new();
    engine.register_provider(mem as Arc<dyn Provider>);
    engine
}

/// `Foo.txt → foo.txt` en un volumen que pliega: los BYTES cambian, así que es
/// un renombrado de verdad. Rechazarlo deja al lector sin salida — la ventana
/// no tiene el modal de colisión con reintento que sí tiene el terminal.
#[tokio::test]
async fn un_rename_que_solo_cambia_la_caja_es_trabajo_de_verdad() {
    let engine = engine_que_pliega().await;
    let handle = engine
        .move_(&vp("mem:///casa/Foo.txt"), &vp("mem:///casa/foo.txt"))
        .await
        .expect("encola");
    assert_eq!(
        handle.join().await,
        TaskState::Completed,
        "cambiar la caja no es renombrar algo sobre sí mismo"
    );
}

/// Y mover algo exactamente sobre sí mismo NO es un error: es un no-op, que es
/// lo que `rename(2)` promete cuando las dos rutas nombran el mismo fichero.
/// Se fija aquí porque es lo que distingue este caso del que la guarda de
/// «dentro de sí mismo» sí tiene que rechazar, y porque un futuro que lo
/// convirtiera en error rompería el renombrado que no cambia nada.
#[tokio::test]
async fn un_rename_sobre_si_mismo_es_un_no_op_y_no_un_error() {
    let engine = engine_que_pliega().await;
    let handle = engine
        .move_(&vp("mem:///casa/Foo.txt"), &vp("mem:///casa/Foo.txt"))
        .await
        .expect("encola");
    assert_eq!(handle.join().await, TaskState::Completed);
}
