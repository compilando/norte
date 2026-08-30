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
//! **Y eso era verdad a medias, porque el doble era más permisivo que
//! cualquier disco.** `MemProvider::rename` permitía `a → A` cuando el destino
//! resolvía al propio origen, modelando el `rename(2)` de APFS. norte no
//! renombra con `rename(2)`: renombra SIN PISAR —`renameat2(RENAME_NOREPLACE)`,
//! `renamex_np(RENAME_EXCL)`, `MoveFileExW` sin replace— y ahí un destino que
//! resuelve al mismo nodo EXISTE, así que el rename falla con `EEXIST`. Con
//! `MemProvider::with_folding_noreplace` (#274) el doble contesta lo que
//! contesta el disco, y entonces sí se ve: el rename se rehúsa con «ya existe»,
//! y con `Overwrite` la secuencia era *borrar el destino* —que es el propio
//! fichero— y renombrar después algo que ya no está.
//!
//! Queda vivo el caso ESTRECHO que sigue sin poder montarse: un move que
//! degrada a copy+delete por EXDEV sobre un volumen que pliega. Pide dos
//! montajes reales, uno de ellos plegando.

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
    monta(MemProvider::with_flags(
        CapabilityFlags::RENAME_ATOMIC | CapabilityFlags::CASE_PRESERVING,
    ))
    .await
    .0
}

/// El mismo, pero cuyo rename SIN PISAR ve el pliegue: lo que hace un disco
/// (#274). Devuelve también el provider, para poder mirar qué quedó.
async fn engine_que_pliega_al_renombrar() -> (Engine, Arc<MemProvider>) {
    monta(
        MemProvider::with_flags(CapabilityFlags::RENAME_ATOMIC | CapabilityFlags::CASE_PRESERVING)
            .with_folding_noreplace(),
    )
    .await
}

async fn monta(mem: MemProvider) -> (Engine, Arc<MemProvider>) {
    let mem = Arc::new(mem);
    mem.mkdir(&vp("mem:///casa")).await.expect("casa");
    escribe(&mem, "mem:///casa/Foo.txt").await;
    let engine = Engine::new();
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    (engine, mem)
}

/// Qué nombres hay en `mem:///casa`, en BYTES y ordenados por bytes.
///
/// No `String`: dos nombres distintos que colapsaran al mismo `U+FFFD`
/// pasarían un `assert_eq!` sin que nadie se enterase, y esto es justo un test
/// sobre nombres (regla 1).
async fn nombres(mem: &MemProvider) -> Vec<Vec<u8>> {
    use futures::StreamExt as _;
    let mut s = mem.list(&vp("mem:///casa")).await.expect("lista");
    let mut out = Vec::new();
    while let Some(e) = s.next().await {
        let e = e.expect("entrada");
        out.push(e.path.file_name().expect("hoja").as_bytes().to_vec());
    }
    out.sort();
    out
}

/// Lo mismo sobre un directorio de disco de verdad.
fn nombres_en(dir: &std::path::Path) -> Vec<Vec<u8>> {
    use std::os::unix::ffi::OsStrExt as _;
    let mut out: Vec<Vec<u8>> = std::fs::read_dir(dir)
        .expect("read_dir")
        .map(|e| e.expect("entrada").file_name().as_bytes().to_vec())
        .collect();
    out.sort();
    out
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

/// **El caso del issue, con el doble contestando lo que contesta un disco.**
///
/// Renombrar SIN PISAR ve el pliegue, así que el destino «ya existe» — y es el
/// propio fichero. Rechazarlo deja al lector sin salida: la ventana no tiene el
/// modal de colisión con reintento que sí tiene el terminal, y la ortografía es
/// lo único que se quería cambiar.
#[tokio::test]
async fn con_un_rename_que_ve_el_pliegue_cambiar_la_caja_sigue_siendo_trabajo() {
    let (engine, mem) = engine_que_pliega_al_renombrar().await;
    let handle = engine
        .move_(&vp("mem:///casa/Foo.txt"), &vp("mem:///casa/foo.txt"))
        .await
        .expect("encola");
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(
        nombres(&mem).await,
        vec![b"foo.txt".to_vec()],
        "un fichero, con la ortografía nueva"
    );
}

/// **Y con `Overwrite` el fichero NO se pierde.**
///
/// Ésta es la que muerde: el brazo de `Overwrite` borraba el destino antes de
/// renombrar, y en un volumen que pliega el destino ES el origen. La secuencia
/// era borrar el fichero y renombrar después algo que ya no estaba — un
/// `Foo.txt → foo.txt` que se lleva el fichero por delante.
#[tokio::test]
async fn con_overwrite_un_cambio_de_caja_no_se_lleva_el_fichero_por_delante() {
    use norte_core::TransferOptions;

    let (engine, mem) = engine_que_pliega_al_renombrar().await;
    let handle = engine
        .move_with(
            &vp("mem:///casa/Foo.txt"),
            &vp("mem:///casa/foo.txt"),
            TransferOptions {
                on_collision: norte_proto::CollisionPolicy::Overwrite,
                ..TransferOptions::default()
            },
        )
        .await
        .expect("encola");
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(
        nombres(&mem).await,
        vec![b"foo.txt".to_vec()],
        "el fichero sigue ahí, con la ortografía nueva"
    );
}

/// Con `Skip`, en cambio, no hay nada que saltar: la «colisión» es el propio
/// fichero, así que el renombrado se hace igual. Saltarlo sería contestar «ya
/// había uno ahí» sobre uno mismo.
#[tokio::test]
async fn con_skip_tampoco_se_salta_el_cambio_de_caja() {
    use norte_core::TransferOptions;

    let (engine, mem) = engine_que_pliega_al_renombrar().await;
    let handle = engine
        .move_with(
            &vp("mem:///casa/Foo.txt"),
            &vp("mem:///casa/foo.txt"),
            TransferOptions {
                on_collision: norte_proto::CollisionPolicy::Skip,
                ..TransferOptions::default()
            },
        )
        .await
        .expect("encola");
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(nombres(&mem).await, vec![b"foo.txt".to_vec()]);
}

/// **Un HARDLINK no es un cambio de ortografía**, aunque comparta inodo.
///
/// `NodeId` es `(dispositivo, inodo)`, así que dos entradas de directorio
/// distintas enlazadas al mismo fichero dan el mismo id. Decidir por identidad
/// SOLA metía `mv a.txt b.txt` por el camino de la ortografía: paso al nombre
/// intermedio, choque con `b.txt` —que sigue ahí—, vuelta atrás, y un
/// `Conflict` donde `Overwrite` hacía lo correcto. Por eso la guarda pide
/// también que las hojas plieguen a la misma clave.
///
/// Va sobre disco de VERDAD porque `MemProvider` no sabe hacer hardlinks.
#[tokio::test]
async fn un_hardlink_no_entra_por_el_camino_de_la_ortografia() {
    use norte_core::TransferOptions;

    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("a.txt"), b"contenido").expect("a");
    std::fs::hard_link(dir.path().join("a.txt"), dir.path().join("b.txt")).expect("enlace");

    let engine = Engine::new();
    engine.register_provider(
        Arc::new(norte_vfs_local::LocalProvider::rooted(dir.path())) as Arc<dyn Provider>
    );
    let handle = engine
        .move_with(
            &vp("file:///a.txt"),
            &vp("file:///b.txt"),
            TransferOptions {
                on_collision: norte_proto::CollisionPolicy::Overwrite,
                ..TransferOptions::default()
            },
        )
        .await
        .expect("encola");
    let estado = handle.join().await;

    // Lo que NO puede pasar: que quede el nombre de máquina por ahí.
    let quedan = nombres_en(dir.path());
    assert!(
        !quedan.iter().any(|n| n.starts_with(b".norte-rename-")),
        "ningún residuo del rodeo: {quedan:?}"
    );
    assert!(
        matches!(estado, TaskState::Completed),
        "con Overwrite, un hardlink es una colisión con política, no un rodeo: {estado:?}"
    );
}

/// **Cancelar entre los dos pasos no deja el fichero con el nombre del
/// rodeo.**
///
/// La ventana no es cancelable a propósito: con el token del task, cancelar
/// justo después del primer rename hacía que el segundo *y la vuelta atrás*
/// salieran sin intentar nada, dejando el fichero con un nombre que el lector
/// no escribió y contestando `Cancelled` — y aquí una task cancelada significa
/// «el árbol está como estaba». Es la misma decisión que `rename::exec`: la
/// cancelación se comprueba ENTRE operaciones, nunca dentro de una.
#[tokio::test]
async fn cancelar_entre_los_dos_pasos_no_deja_el_nombre_del_rodeo() {
    let mem = Arc::new(
        MemProvider::with_flags(CapabilityFlags::RENAME_ATOMIC | CapabilityFlags::CASE_PRESERVING)
            .with_folding_noreplace(),
    );
    mem.mkdir(&vp("mem:///casa")).await.expect("casa");
    escribe(&mem, "mem:///casa/Foo.txt").await;
    let engine = Engine::new();
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);

    let handle = engine
        .move_(&vp("mem:///casa/Foo.txt"), &vp("mem:///casa/foo.txt"))
        .await
        .expect("encola");
    // El primer rename es el del rodeo: en cuanto se aplica, se cancela.
    mem.faults().cancel_after_renames(1, handle.cancel_token());
    let estado = handle.join().await;

    let quedan = nombres(&mem).await;
    assert!(
        !quedan.iter().any(|n| n.starts_with(b".norte-rename-")),
        "ni cancelando queda el nombre del rodeo: {quedan:?}"
    );
    assert_eq!(
        estado,
        TaskState::Completed,
        "la ventana entre los dos pasos no es cancelable"
    );
}

/// Un nombre pegado al límite de 255 bytes también se puede cambiar de caja.
///
/// El nombre intermedio va por PREFIJO y no empotra la hoja: con un sufijo,
/// una hoja de 250 bytes daba `ENAMETOOLONG` en el primer paso y el cambio de
/// ortografía era imposible. Es lo que la fixture `name_max_255` del corpus
/// dice desde que existe.
#[tokio::test]
async fn una_hoja_al_limite_de_255_tambien_cambia_de_caja() {
    let largo = "A".repeat(251);
    let mem = Arc::new(
        MemProvider::with_flags(CapabilityFlags::RENAME_ATOMIC | CapabilityFlags::CASE_PRESERVING)
            .with_folding_noreplace(),
    );
    mem.mkdir(&vp("mem:///casa")).await.expect("casa");
    escribe(&mem, &format!("mem:///casa/{largo}.txt")).await;
    let engine = Engine::new();
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);

    let handle = engine
        .move_(
            &vp(&format!("mem:///casa/{largo}.txt")),
            &vp(&format!("mem:///casa/{}.txt", largo.to_lowercase())),
        )
        .await
        .expect("encola");
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(
        nombres(&mem).await,
        vec![format!("{}.txt", largo.to_lowercase()).into_bytes()]
    );
}

/// Y una colisión de VERDAD sigue siendo una colisión: dos ficheros distintos,
/// y el de destino no se toca. Sin esto, «trátalo como un cambio de caja»
/// pasaría a significar «pisa lo que haya».
#[tokio::test]
async fn una_colision_de_verdad_sigue_fallando() {
    let (engine, mem) = engine_que_pliega_al_renombrar().await;
    escribe(&mem, "mem:///casa/otro.txt").await;
    let handle = engine
        .move_(&vp("mem:///casa/Foo.txt"), &vp("mem:///casa/otro.txt"))
        .await
        .expect("encola");
    assert!(
        matches!(handle.join().await, TaskState::Failed { .. }),
        "dos ficheros distintos siguen colisionando"
    );
    assert_eq!(
        nombres(&mem).await,
        vec![b"Foo.txt".to_vec(), b"otro.txt".to_vec()]
    );
}
