//! #219/#218 de punta a punta contra el sistema de ficheros REAL: una copia de
//! UNA hoja tampoco se sale de su destino, ni escribiendo ni borrando.
//!
//! Va con `file://` a propósito, por lo mismo que `engine_sync_confined`:
//! `MemProvider` no tiene componentes intermedios que seguir ni `openat` con el
//! que negarse, así que el agujero solo existe —y solo se puede demostrar
//! cerrado— contra un filesystem de verdad.
//!
//! Lo que se prueba aquí es la operación MÁS COMÚN del producto. Hasta #219,
//! `ops::copy_task` usaba `Destination::unconfined` para una hoja suelta, con
//! el argumento de que «una hoja no cuelga de ningún árbol aprobado». Cuelga de
//! uno: su DIRECTORIO destino, que es lo que el panel enseñaba y lo que el
//! diálogo nombra.
//!
//! **#219 queda ESTRECHADO, no cerrado, y el test que lo dice está abajo.** Un
//! enlace que ya estaba puesto cuando el core mira por primera vez sigue
//! desviando la copia, porque desde el core es idéntico a un
//! `~/copias -> /mnt/disco/copias` legítimo. Lo que se gana es que el
//! directorio se resuelve UNA vez en lugar de tres más los reintentos, y que
//! un cambiazo posterior ya no desvía nada.

#![cfg(unix)]

use std::sync::Arc;

use norte_core::{Actor, Engine};
use norte_proto::{CollisionPolicy, TaskState, VPath};
use norte_vfs::Provider;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("wire válido")
}

struct Arbol {
    engine: Engine,
    dir: tempfile::TempDir,
}

/// Un origen, un destino con un `sub/` dentro, y un `fuera/` hermano del
/// destino — los tres bajo la raíz del provider, que es lo que hace de esto una
/// fuga del DESTINO y no un fallo de la raíz del provider.
fn arbol() -> Arbol {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("origen.txt"), b"contenido").expect("origen");
    std::fs::create_dir(dir.path().join("d")).expect("destino");
    std::fs::create_dir(dir.path().join("fuera")).expect("fuera");
    let engine = Engine::new();
    engine.register_provider(
        Arc::new(norte_vfs_local::LocalProvider::rooted(dir.path())) as Arc<dyn Provider>
    );
    Arbol { engine, dir }
}

async fn copia(a: &Arbol, to: &str, policy: CollisionPolicy) -> TaskState {
    let handle = a
        .engine
        .copy_with_as(
            &vp("file:///origen.txt"),
            &vp(to),
            norte_core::TransferOptions {
                on_collision: policy,
                ..norte_core::TransferOptions::default()
            },
            Actor::User,
        )
        .await
        .expect("encola");
    handle.join().await
}

/// **El residuo de #219, escrito para que nadie lo lea de más.**
///
/// Con `d/sub -> fuera` YA plantado cuando el core mira por primera vez, la
/// copia SÍ aterriza fuera. No es un descuido: desde el core, ese enlace y un
/// `~/copias -> /mnt/disco/copias` legítimo son indistinguibles —los dos
/// resuelven a otro sitio— y rechazar los dos rompería la copia a `/tmp` en
/// macOS y a media distribución con usrmerge.
///
/// Lo que #219 sí compra para una hoja es que, una vez resuelto ese
/// directorio UNA vez, el staging y su publicación van los dos por el
/// descriptor: donde antes había tres resoluciones de `dest/sub` más una por
/// cada uno de los tres reintentos, ahora hay una. Un cambiazo POSTERIOR ya no
/// desvía nada.
///
/// Cerrar la otra mitad pide la identidad que se observó al APROBAR —el
/// listado que el humano miró— viajando con la petición, y eso es wire.
#[tokio::test]
async fn un_enlace_ya_plantado_en_el_destino_no_lo_puede_distinguir_el_core() {
    let a = arbol();
    let fuera = a.dir.path().join("fuera");
    std::os::unix::fs::symlink(&fuera, a.dir.path().join("d/sub")).expect("enlace");

    let estado = copia(&a, "file:///d/sub/botin.txt", CollisionPolicy::Fail).await;

    assert_eq!(
        estado,
        TaskState::Completed,
        "la copia se hace: el core no puede saber que este enlace no es legítimo"
    );
    assert!(
        fuera.join("botin.txt").exists(),
        "y aterriza donde el enlace apunta — que es lo que hace un enlace"
    );
}

/// Y lo normal sigue funcionando: una hoja a un destino honesto se copia. El
/// confinamiento no puede costar la operación que existe para proteger.
#[tokio::test]
async fn una_copia_de_una_hoja_a_un_destino_honesto_sigue_funcionando() {
    let a = arbol();
    std::fs::create_dir(a.dir.path().join("d/sub")).expect("sub de verdad");

    let estado = copia(&a, "file:///d/sub/copia.txt", CollisionPolicy::Fail).await;

    assert_eq!(estado, TaskState::Completed, "{estado:?}");
    assert_eq!(
        std::fs::read(a.dir.path().join("d/sub/copia.txt")).expect("llegó"),
        b"contenido"
    );
}

/// Y `Overwrite` sobre un destino honesto reemplaza, que es lo que promete.
#[tokio::test]
async fn una_copia_con_overwrite_a_un_destino_honesto_reemplaza() {
    let a = arbol();
    std::fs::create_dir(a.dir.path().join("d/sub")).expect("sub de verdad");
    let destino = a.dir.path().join("d/sub/copia.txt");
    std::fs::write(&destino, b"lo viejo").expect("previo");

    let estado = copia(&a, "file:///d/sub/copia.txt", CollisionPolicy::Overwrite).await;

    assert_eq!(estado, TaskState::Completed, "{estado:?}");
    assert_eq!(std::fs::read(&destino).expect("llegó"), b"contenido");
}

/// Un symlink como hoja va por el mismo camino que un fichero: copiarlo es
/// CREAR uno en el destino, y esa creación compone una ruta igual que las
/// otras dos. Se comprueba que el camino confinado no lo rompe.
#[tokio::test]
async fn una_copia_de_un_symlink_hoja_a_un_destino_honesto_funciona() {
    let a = arbol();
    std::os::unix::fs::symlink("origen.txt", a.dir.path().join("enlace")).expect("enlace");
    std::fs::create_dir(a.dir.path().join("d/sub")).expect("sub de verdad");

    let handle = a
        .engine
        .copy_with_as(
            &vp("file:///enlace"),
            &vp("file:///d/sub/copia"),
            norte_core::TransferOptions::default(),
            Actor::User,
        )
        .await
        .expect("encola");

    assert_eq!(handle.join().await, TaskState::Completed);
    let meta = std::fs::symlink_metadata(a.dir.path().join("d/sub/copia")).expect("llegó");
    assert!(
        meta.file_type().is_symlink(),
        "y sigue siendo un enlace: `Preserve` no lo dereferencia"
    );
}

/// #218 al nivel del engine: con `Overwrite`, el `stat` que DECIDE y el
/// `remove` que EJECUTA van los dos por el descriptor cuando hay raíz.
///
/// Lo que este test demuestra desde aquí es que el camino confinado hace el
/// trabajo correcto. Que se NIEGA a salirse lo demuestra el provider, donde el
/// descriptor es observable:
/// `norte-vfs-local/tests/confined.rs`, `un_symlink_intermedio_no_redirige_un_borrado_fuera_de_la_raiz`.
#[tokio::test]
async fn una_copia_con_overwrite_borra_y_reemplaza_por_el_descriptor() {
    let a = arbol();
    std::fs::create_dir(a.dir.path().join("d/sub")).expect("sub de verdad");
    let destino = a.dir.path().join("d/sub/botin.txt");
    std::fs::write(&destino, b"lo viejo").expect("previo");

    let estado = copia(&a, "file:///d/sub/botin.txt", CollisionPolicy::Overwrite).await;

    assert_eq!(estado, TaskState::Completed, "{estado:?}");
    assert_eq!(std::fs::read(&destino).expect("llegó"), b"contenido");
}

/// Un directorio destino que ES un enlace sigue funcionando (#219).
///
/// `~/copias -> /mnt/disco/copias` es un destino legítimo y corriente; en
/// macOS lo son `/tmp`, `/var` y `/etc`, y en un Linux con usrmerge `/bin` y
/// `/lib`. La primera versión de este cambio los rechazaba a todos con
/// `EscapesRoot` —la comprobación de identidad ve el enlace por un lado y el
/// directorio real por el otro— y lo destapó la revisión de seguridad. Ahora
/// se confina igual y lo único que se salta es esa comprobación, que sobre un
/// enlace no podía decir nada.
#[tokio::test]
async fn un_directorio_destino_que_es_un_enlace_sigue_funcionando() {
    let a = arbol();
    let real = a.dir.path().join("almacen");
    std::fs::create_dir(&real).expect("almacen");
    std::os::unix::fs::symlink(&real, a.dir.path().join("d/enlazado")).expect("enlace legítimo");

    let estado = copia(&a, "file:///d/enlazado/copia.txt", CollisionPolicy::Fail).await;

    assert_eq!(
        estado,
        TaskState::Completed,
        "un `~/copias -> /mnt/disco` legítimo no puede dejar de funcionar: {estado:?}"
    );
    assert_eq!(
        std::fs::read(real.join("copia.txt")).expect("llegó al sitio real"),
        b"contenido"
    );
}

/// Y una copia RECURSIVA hacia un directorio enlazado tampoco se rompe: su
/// raíz la abre `copy_tree` sobre el `to` que acaba de crear, y desde que
/// `open_leaf_root` vive DENTRO de los brazos de hoja ya no hereda la de éstos
/// ni paga sus tres syscalls.
#[tokio::test]
async fn un_arbol_hacia_un_directorio_enlazado_sigue_funcionando() {
    let a = arbol();
    std::fs::create_dir(a.dir.path().join("arbol")).expect("arbol");
    std::fs::write(a.dir.path().join("arbol/hoja.txt"), b"dentro").expect("hoja");
    let real = a.dir.path().join("almacen");
    std::fs::create_dir(&real).expect("almacen");
    std::os::unix::fs::symlink(&real, a.dir.path().join("d/enlazado")).expect("enlace legítimo");

    let handle = a
        .engine
        .copy_with_as(
            &vp("file:///arbol"),
            &vp("file:///d/enlazado/copia"),
            norte_core::TransferOptions::default(),
            Actor::User,
        )
        .await
        .expect("encola");

    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(
        std::fs::read(real.join("copia/hoja.txt")).expect("el árbol llegó"),
        b"dentro"
    );
}
