//! El ancla del directorio destino (#295, ADR 0073) contra el filesystem REAL.
//!
//! Es la mitad que ADR 0072 dejaba abierta: un enlace **ya plantado** cuando el
//! core mira por primera vez. Desde dentro del core, ese enlace y un
//! `~/copias -> /mnt/disco/copias` legítimo son idénticos —los dos resuelven a
//! otro sitio—, así que quien los distingue tiene que ser quien MIRÓ: el
//! cliente que listó el directorio y retuvo su identidad.
//!
//! Va con `file://` por lo mismo que `engine_leaf_confined`: `MemProvider` no
//! tiene enlaces que seguir ni identidad de nodo que comparar.
#![cfg(unix)]

use std::sync::Arc;

use norte_core::{Actor, Engine, TransferOptions};
use norte_proto::{CollisionPolicy, DirAnchor, TaskState, VPath};
use norte_vfs::Provider;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("wire válido")
}

struct Arbol {
    engine: Engine,
    dir: tempfile::TempDir,
}

/// Un origen (fichero y árbol), un destino `d/` con un `sub/` REAL dentro, y un
/// `fuera/` hermano al que un atacante querría desviar la escritura.
fn arbol() -> Arbol {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("origen.txt"), b"contenido").expect("origen");
    std::fs::create_dir(dir.path().join("arbol")).expect("árbol origen");
    std::fs::write(dir.path().join("arbol/hoja.txt"), b"hoja").expect("hoja");
    std::fs::create_dir(dir.path().join("d")).expect("destino");
    std::fs::create_dir(dir.path().join("d/sub")).expect("sub");
    std::fs::create_dir(dir.path().join("fuera")).expect("fuera");
    let engine = Engine::new();
    engine.register_provider(
        Arc::new(norte_vfs_local::LocalProvider::rooted(dir.path())) as Arc<dyn Provider>
    );
    Arbol { engine, dir }
}

/// Lo que hace el cliente al LISTAR: retener la identidad de lo que mira.
async fn ancla(a: &Arbol, dir: &str) -> DirAnchor {
    a.engine
        .dir_anchor(&vp(dir))
        .await
        .expect("preguntar")
        .expect("el provider local sabe identificar nodos")
}

/// La forma EXACTA del rechazo: un conflicto que dice «esto se sale de la raíz
/// aprobada», no un error de E/S ni un `NotFound`. Los frontends pintan por
/// categoría, así que la categoría es contrato.
#[track_caller]
fn rehusado(estado: &TaskState) {
    assert!(
        matches!(
            estado,
            TaskState::Failed {
                error: norte_proto::Error::Conflict {
                    conflict: norte_proto::ConflictKind::EscapesRoot
                }
            }
        ),
        "tenía que rehusar por identidad del destino, fue {estado:?}"
    );
}

async fn copia(a: &Arbol, from: &str, to: &str, anchor: Option<DirAnchor>) -> TaskState {
    let handle = a
        .engine
        .copy_anchored(
            &vp(from),
            &vp(to),
            TransferOptions {
                on_collision: CollisionPolicy::Fail,
                ..TransferOptions::default()
            },
            Actor::User,
            anchor,
        )
        .await
        .expect("encola");
    handle.join().await
}

/// **El caso del issue.** El humano listó `d/sub` —un directorio de verdad— y
/// aprobó copiar ahí. Para cuando la copia corre, `d/sub` es un enlace a
/// `fuera`. El core sigue sin poder distinguirlo de un enlace legítimo; el
/// ancla sí, porque no habla de enlaces sino de NODOS.
#[tokio::test]
async fn un_destino_sustituido_por_un_enlace_ya_no_recibe_la_copia() {
    let a = arbol();
    let visto = ancla(&a, "file:///d/sub").await;

    std::fs::remove_dir(a.dir.path().join("d/sub")).expect("quitar el de verdad");
    std::os::unix::fs::symlink(a.dir.path().join("fuera"), a.dir.path().join("d/sub"))
        .expect("plantar el enlace");

    let estado = copia(
        &a,
        "file:///origen.txt",
        "file:///d/sub/botin.txt",
        Some(visto),
    )
    .await;

    rehusado(&estado);
    assert!(
        !a.dir.path().join("fuera/botin.txt").exists(),
        "y NADA aterrizó al otro lado del enlace"
    );
}

/// Sin ancla, lo de 0.53: la copia se hace y aterriza donde el enlace apunta.
/// Está aquí para que la diferencia sea del ANCLA y no de otra cosa que
/// cambiara a la vez.
#[tokio::test]
async fn sin_ancla_el_mismo_enlace_sigue_desviando_la_copia() {
    let a = arbol();
    std::fs::remove_dir(a.dir.path().join("d/sub")).expect("quitar");
    std::os::unix::fs::symlink(a.dir.path().join("fuera"), a.dir.path().join("d/sub"))
        .expect("enlace");

    let estado = copia(&a, "file:///origen.txt", "file:///d/sub/botin.txt", None).await;

    assert_eq!(estado, TaskState::Completed);
    assert!(
        a.dir.path().join("fuera/botin.txt").exists(),
        "el residuo de ADR 0072, intacto cuando nadie manda ancla"
    );
}

/// Y el ancla correcta no cuesta la operación: mismo nodo, la copia se hace.
#[tokio::test]
async fn el_ancla_del_directorio_de_verdad_deja_copiar() {
    let a = arbol();
    let visto = ancla(&a, "file:///d/sub").await;

    let estado = copia(
        &a,
        "file:///origen.txt",
        "file:///d/sub/copia.txt",
        Some(visto),
    )
    .await;

    assert_eq!(estado, TaskState::Completed);
    assert!(a.dir.path().join("d/sub/copia.txt").exists());
}

/// **Un enlace LEGÍTIMO no se rompe**, que es la razón por la que ADR 0072 no
/// pudo comparar identidades a secas: en macOS `/tmp`, `/var` y `/etc` son
/// enlaces, y en un Linux con usrmerge lo son `/bin` y `/lib`.
///
/// El ancla se saca SIGUIENDO el enlace, así que listar `enlace/` y listar
/// `d/sub/` dan la misma: el mismo destino por dos nombres no puede ser dos
/// destinos.
#[tokio::test]
async fn un_enlace_legitimo_al_directorio_aprobado_pasa() {
    let a = arbol();
    std::os::unix::fs::symlink(a.dir.path().join("d/sub"), a.dir.path().join("enlace"))
        .expect("enlace legítimo");

    let por_el_enlace = ancla(&a, "file:///enlace").await;
    assert_eq!(
        por_el_enlace,
        ancla(&a, "file:///d/sub").await,
        "el mismo nodo por dos nombres da la misma ancla"
    );

    let estado = copia(
        &a,
        "file:///origen.txt",
        "file:///enlace/copia.txt",
        Some(por_el_enlace),
    )
    .await;

    assert_eq!(estado, TaskState::Completed);
    assert!(a.dir.path().join("d/sub/copia.txt").exists());
}

/// Un ancla que nadie emitió no autoriza nada. Es la propiedad que hace que el
/// secreto del proceso importe: sin él, quien conozca el formato la fabricaría.
#[tokio::test]
async fn un_ancla_inventada_no_autoriza() {
    let a = arbol();
    let estado = copia(
        &a,
        "file:///origen.txt",
        "file:///d/sub/copia.txt",
        Some(DirAnchor::new(
            "0123456789abcdef0123456789abcdef".to_owned(),
        )),
    )
    .await;

    rehusado(&estado);
    assert!(!a.dir.path().join("d/sub/copia.txt").exists());
}

/// Un ÁRBOL crea su destino, así que el ancla se comprueba sobre el padre y
/// por ruta. Lo que se demuestra aquí es que también se comprueba: sin esto,
/// copiar un directorio se quedaba sin la defensa que gana copiar un fichero.
#[tokio::test]
async fn un_arbol_hacia_un_padre_sustituido_tampoco_se_copia() {
    let a = arbol();
    let visto = ancla(&a, "file:///d/sub").await;

    std::fs::remove_dir(a.dir.path().join("d/sub")).expect("quitar");
    std::os::unix::fs::symlink(a.dir.path().join("fuera"), a.dir.path().join("d/sub"))
        .expect("enlace");

    let estado = copia(&a, "file:///arbol", "file:///d/sub/arbol", Some(visto)).await;

    rehusado(&estado);
    assert!(
        !a.dir.path().join("fuera/arbol").exists(),
        "ni siquiera se creó el directorio raíz del árbol"
    );
}

/// Y mover por copia lo hereda: un `fs.move` que degrada a copy+delete escribe
/// igual que una copia, y además borra el origen después.
#[tokio::test]
async fn mover_a_un_destino_sustituido_ni_escribe_ni_borra_el_origen() {
    let a = arbol();
    let visto = ancla(&a, "file:///d/sub").await;
    std::fs::remove_dir(a.dir.path().join("d/sub")).expect("quitar");
    std::os::unix::fs::symlink(a.dir.path().join("fuera"), a.dir.path().join("d/sub"))
        .expect("enlace");

    // Cross-provider no hace falta: lo que se prueba es el camino anclado del
    // move, y `move_anchored` lo lleva hasta `move_by_copy` igual.
    let handle = a
        .engine
        .move_anchored(
            &vp("file:///origen.txt"),
            &vp("file:///d/sub/botin.txt"),
            TransferOptions {
                on_collision: CollisionPolicy::Fail,
                ..TransferOptions::default()
            },
            Actor::User,
            Some(visto),
        )
        .await
        .expect("encola")
        .join()
        .await;

    rehusado(&handle);
    assert!(
        !a.dir.path().join("fuera/botin.txt").exists(),
        "no escribió al otro lado"
    );
    assert!(
        a.dir.path().join("origen.txt").exists(),
        "y no borró el origen: un move que no coloca no borra"
    );
}

/// **Crear un fichero también va anclado** (#290), y es donde el ancla vale
/// MÁS, no menos.
///
/// `fs.create` es el único método del wire cuyo éxito entrega una ruta a un
/// programa de FUERA de norte: la ventana crea el fichero para abrirlo con el
/// editor del escritorio. Con el enlace plantado entre el listado y la
/// confirmación no se pierde un fichero vacío — se pierde la sesión de edición
/// entera que el humano escribe después, en un directorio que él no estaba
/// mirando.
#[tokio::test]
async fn crear_un_fichero_en_un_destino_sustituido_se_rehusa() {
    let a = arbol();
    let visto = ancla(&a, "file:///d/sub").await;

    // El atacante cambia `d/sub` por un enlace a `fuera/`.
    std::fs::remove_dir(a.dir.path().join("d/sub")).expect("quitar sub");
    std::os::unix::fs::symlink(a.dir.path().join("fuera"), a.dir.path().join("d/sub"))
        .expect("plantar el enlace");

    let estado = a
        .engine
        .create_file_as(&vp("file:///d/sub/borrador.md"), Some(visto), Actor::User)
        .await
        .expect("encola")
        .join()
        .await;

    rehusado(&estado);
    assert!(
        !a.dir.path().join("fuera/borrador.md").exists(),
        "no creó nada al otro lado del enlace"
    );
}

/// Y sin ancla se comporta como antes de #295: se crea donde diga la ruta.
///
/// La comprobación es una MEJORA que quien lista puede pedir, no un requisito
/// nuevo — un `norte` contra una ruta tecleada a mano sigue funcionando.
#[tokio::test]
async fn crear_sin_ancla_sigue_creando() {
    let a = arbol();
    let estado = a
        .engine
        .create_file_as(&vp("file:///d/sub/borrador.md"), None, Actor::User)
        .await
        .expect("encola")
        .join()
        .await;

    assert_eq!(estado, TaskState::Completed, "{estado:?}");
    assert!(a.dir.path().join("d/sub/borrador.md").is_file());
}
