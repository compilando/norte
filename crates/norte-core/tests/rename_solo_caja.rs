//! Renombrar cambiando SOLO la caja, sobre un destino que pliega (#274).
//!
//! `Foo.txt → foo.txt` en APFS, HFS+, NTFS, exFAT o un SMB que pliegue es
//! trabajo de verdad: el filesystem guarda el nombre nuevo aunque los dos
//! nombres se refieran al mismo inodo. Lo mismo NFD → NFC.
//!
//! El planificador de lotes ya lo sabe —`rename/plan.rs` tiene
//! `a_case_only_rename_is_real_work_on_a_case_insensitive_directory` y emite
//! un paso de verdad—, así que una ventana con dos renombrados no puede dar
//! respuestas opuestas ante la misma entrada.

use std::sync::Arc;

use norte_core::{Actor, Engine, TransferOptions};
use norte_proto::{CapabilityFlags, CollisionPolicy, TaskState, VPath};
use norte_testkit::{MemProvider, Normalization};
use norte_vfs::Provider;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("wire válido")
}

/// Un provider que PLIEGA, como APFS: sin `CASE_SENSITIVE` y con la
/// normalización insensible, o sea que `A.txt` y `a.txt` son el mismo nodo.
fn engine_que_pliega() -> (Engine, Arc<MemProvider>) {
    let caps = MemProvider::new().capabilities().flags & !CapabilityFlags::CASE_SENSITIVE;
    let mem =
        Arc::new(MemProvider::with_flags(caps).with_normalization(Normalization::Insensitive));
    let engine = Engine::new();
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    (engine, mem)
}

async fn escribe(mem: &MemProvider, wire: &str, bytes: &[u8]) {
    let mut sink = mem.write(&vp(wire)).await.expect("write abre");
    sink.write(bytes::Bytes::copy_from_slice(bytes))
        .await
        .expect("chunk");
    sink.commit().await.expect("commit");
}

async fn mueve(engine: &Engine, from: &str, to: &str) -> TaskState {
    let handle = engine
        .move_with_as(
            &vp(from),
            &vp(to),
            TransferOptions {
                on_collision: CollisionPolicy::Fail,
                ..TransferOptions::default()
            },
            Actor::User,
        )
        .await
        .expect("encola");
    handle.join().await
}

/// **El caso del issue.** Cambiar solo la caja NO es mover algo sobre sí
/// mismo: es el rename que hace `F2` en cualquier gestor de archivos.
#[tokio::test]
async fn un_rename_que_solo_cambia_la_caja_se_hace() {
    let (engine, mem) = engine_que_pliega();
    escribe(&mem, "mem:///Foo.txt", b"contenido").await;

    assert_eq!(
        mueve(&engine, "mem:///Foo.txt", "mem:///foo.txt").await,
        TaskState::Completed,
        "el mismo nodo con otro nombre es trabajo real, no autodestrucción"
    );
}

/// **La misma ruta byte a byte NO se rechaza: el rename es un no-op que sale
/// bien**, igual que `rename(2)` con `old` y `new` apuntando al mismo fichero,
/// que POSIX define como éxito sin hacer nada.
///
/// Es una asimetría con `fs.copy`, que ante `from == to` devuelve
/// `InvalidPath` — y ahí sí hace falta, porque copiar SOBRE sí mismo con
/// `Overwrite` borra el destino antes de leer el origen. Un rename no lee
/// nada, así que no hay nada que destruir. El test la fija para que nadie
/// «arregle» una de las dos hacia la otra sin querer.
#[tokio::test]
async fn moverse_sobre_si_mismo_es_un_no_op_que_sale_bien() {
    let (engine, mem) = engine_que_pliega();
    escribe(&mem, "mem:///Foo.txt", b"contenido").await;

    assert_eq!(
        mueve(&engine, "mem:///Foo.txt", "mem:///Foo.txt").await,
        TaskState::Completed
    );
    let e = mem.stat(&vp("mem:///Foo.txt")).await.expect("sigue ahí");
    assert_eq!(e.size, Some(9), "y no se ha perdido nada por el camino");
}

/// Y el fichero sigue estando, con su contenido: un rename de solo-caja que
/// se resolviera como «lo mismo, no hago nada» y uno que borrara el origen se
/// leen igual desde fuera si nadie mira el contenido.
#[tokio::test]
async fn el_contenido_sobrevive_al_rename_de_solo_caja() {
    let (engine, mem) = engine_que_pliega();
    escribe(&mem, "mem:///Foo.txt", b"contenido").await;

    assert_eq!(
        mueve(&engine, "mem:///Foo.txt", "mem:///foo.txt").await,
        TaskState::Completed
    );
    let e = mem.stat(&vp("mem:///foo.txt")).await.expect("sigue ahí");
    assert_eq!(e.size, Some(9));
}
