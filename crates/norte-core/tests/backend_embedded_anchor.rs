//! El ancla del destino por el camino EMBEBIDO (#301, ADR 0073/0076).
//!
//! `engine_dest_anchor` prueba que el ENGINE rehúsa cuando alguien le pasa el
//! ancla. Esto prueba la otra mitad, que es la que faltaba: que el
//! `Backend::Embedded` la PASA — recordando, como hace el SDK sobre el wire, la
//! identidad de cada directorio que él mismo listó.
//!
//! Importa porque `ntc` corre embebido por defecto (`--daemon` es la
//! excepción), así que sin esto el frontend con más motivo para la comprobación
//! —el que lanza `$EDITOR` sobre el fichero que `fs.create` acaba de crear— era
//! justo el que no la tenía.
//!
//! `file://` y no `MemProvider` por lo mismo que el otro fichero: sin enlaces
//! que seguir ni identidad de nodo que comparar no hay nada que probar.
#![cfg(unix)]

use std::sync::Arc;

use norte_core::backend::Backend;
use norte_core::{Engine, TransferOptions};
use norte_proto::{TaskState, VPath};
use norte_vfs::Provider;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("wire válido")
}

struct Arbol {
    backend: Backend,
    dir: tempfile::TempDir,
}

/// Un origen, un destino `d/sub` de verdad, y un `fuera/` al que un atacante
/// querría desviar la escritura.
fn arbol() -> Arbol {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("origen.txt"), b"contenido").expect("origen");
    std::fs::create_dir(dir.path().join("d")).expect("destino");
    std::fs::create_dir(dir.path().join("d/sub")).expect("sub");
    std::fs::create_dir(dir.path().join("fuera")).expect("fuera");
    // Con la memoria de anclas de un cliente, que es lo que instala
    // `embedded::engine_in` y por tanto lo que tiene el engine de un frontend
    // (#317). Un engine sin ella no ancla nada — eso lo fija su propio test.
    let engine = Engine::new().with_client_anchors();
    engine.register_provider(
        Arc::new(norte_vfs_local::LocalProvider::rooted(dir.path())) as Arc<dyn Provider>
    );
    Arbol {
        backend: Backend::Embedded(Arc::new(engine)),
        dir,
    }
}

/// Lo que hace un PANEL al abrir un directorio: listarlo y retener su ancla.
///
/// Las dos cosas, y por separado, porque el backend no ancla todo lo que lista:
/// el árbol lateral y el `fs.list` de un script también pasan por `list`, y
/// como recordar sobrescribe, cualquiera de ellos rebendeciría el ancla del
/// panel con lo que viera en ese momento (#301).
async fn listar_como_un_panel(backend: &Backend, dir: &str) {
    backend.list(&vp(dir)).await.expect("lista");
    backend.remember_listing_anchor(&vp(dir)).await;
}

/// Quitar el directorio de verdad y dejar un enlace a `fuera` con su nombre:
/// el ataque entero, en dos syscalls.
fn sustituir_por_enlace(a: &Arbol) {
    std::fs::remove_dir(a.dir.path().join("d/sub")).expect("quitar el de verdad");
    std::os::unix::fs::symlink(a.dir.path().join("fuera"), a.dir.path().join("d/sub"))
        .expect("plantar el enlace");
}

/// La forma EXACTA del rechazo, igual que en `engine_dest_anchor`: los
/// frontends pintan por categoría, así que la categoría es contrato.
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

/// **El caso del issue, sobre `fs.create`.** El panel listó `d/sub`; entre eso
/// y el Enter del diálogo, `d/sub` pasó a ser un enlace a `fuera`. El fichero
/// NO se crea al otro lado — que es lo que el `$EDITOR` habría abierto.
#[tokio::test]
async fn crear_en_un_destino_sustituido_por_un_enlace_se_rehusa() {
    let a = arbol();
    // Lo que hace un panel al abrir el directorio, y lo único que hace falta
    // para que el ancla exista: listarlo por el backend.
    listar_como_un_panel(&a.backend, "file:///d/sub").await;

    sustituir_por_enlace(&a);

    let task = a
        .backend
        .create_file(&vp("file:///d/sub/notas.txt"))
        .await
        .expect("encola");
    rehusado(&task.join().await);
    assert!(
        !a.dir.path().join("fuera/notas.txt").exists(),
        "y nada aterrizó al otro lado del enlace"
    );
}

/// Y lo mismo copiando: `pane.copy` sobre el panel que se listó.
#[tokio::test]
async fn copiar_a_un_destino_sustituido_por_un_enlace_se_rehusa() {
    let a = arbol();
    listar_como_un_panel(&a.backend, "file:///d/sub").await;

    sustituir_por_enlace(&a);

    let task = a
        .backend
        .copy(
            &vp("file:///origen.txt"),
            &vp("file:///d/sub/botin.txt"),
            TransferOptions::default(),
        )
        .await
        .expect("encola");
    rehusado(&task.join().await);
    assert!(!a.dir.path().join("fuera/botin.txt").exists());
}

/// Y moviendo, que es la otra escritura anclada.
#[tokio::test]
async fn mover_a_un_destino_sustituido_por_un_enlace_se_rehusa() {
    let a = arbol();
    listar_como_un_panel(&a.backend, "file:///d/sub").await;

    sustituir_por_enlace(&a);

    let task = a
        .backend
        .move_(
            &vp("file:///origen.txt"),
            &vp("file:///d/sub/botin.txt"),
            TransferOptions::default(),
        )
        .await
        .expect("encola");
    rehusado(&task.join().await);
    assert!(!a.dir.path().join("fuera/botin.txt").exists());
    assert!(
        a.dir.path().join("origen.txt").exists(),
        "y el origen sigue donde estaba: un move rehusado no borra nada"
    );
}

/// **Un listado que NO es una pantalla no rebendice el ancla** (#301).
///
/// El árbol lateral pide una rama por vuelta del bucle, y un script Lua puede
/// llamar a `fs.list` cuando quiera. Si esos listados escribieran la caché,
/// bastaría con que uno pasara por el destino DESPUÉS del cambiazo para que la
/// copia del humano pasara la comprobación contra el nodo del atacante.
#[tokio::test]
async fn un_listado_que_no_es_de_panel_no_rebendice_el_ancla() {
    let a = arbol();
    listar_como_un_panel(&a.backend, "file:///d/sub").await;

    sustituir_por_enlace(&a);
    // El árbol lateral pasa por ahí y ve el enlace ya puesto.
    a.backend.list(&vp("file:///d/sub")).await.expect("lista");

    let task = a
        .backend
        .create_file(&vp("file:///d/sub/notas.txt"))
        .await
        .expect("encola");
    rehusado(&task.join().await);
}

/// Sin haber listado el destino no hay ancla que mandar, y entonces esto se
/// comporta como 0.53. Está aquí para que la diferencia sea del ANCLA y no de
/// otra cosa: es el mismo enlace y el mismo backend.
#[tokio::test]
async fn sin_listar_el_destino_no_hay_ancla_y_el_enlace_desvia() {
    let a = arbol();
    sustituir_por_enlace(&a);

    let task = a
        .backend
        .create_file(&vp("file:///d/sub/notas.txt"))
        .await
        .expect("encola");
    assert_eq!(task.join().await, TaskState::Completed);
    assert!(a.dir.path().join("fuera/notas.txt").exists());
}

/// Y el del FRONTEND sí la tiene, que es la otra mitad y la que se puede
/// borrar sin que nada se ponga rojo (#317).
///
/// `embedded::engine_in` es la única constructora que llama a
/// `with_client_anchors`. Sin este test, quitarle esa llamada deja la TUI y la
/// CLI sin comprobación de ancla en silencio.
#[test]
fn el_engine_de_un_frontend_si_ancla() {
    let dir = tempfile::tempdir().expect("tempdir");
    assert!(norte_core::embedded::engine_in(dir.path()).has_client_anchors());
    assert!(
        !Engine::new().has_client_anchors(),
        "y el que no pasa por ahí, no"
    );
}

/// **El engine del DAEMON no puede anclar, monte quien monte un
/// `Backend::Embedded` encima** (#317).
///
/// El ancla dice quién MIRÓ, y eso solo significa algo en un proceso con un
/// cliente. En el daemon hay muchos, así que una caché compartida pasaría el
/// listado del cliente A a la escritura del cliente B. La propiedad la sostiene
/// el tipo —la memoria la instala `with_client_anchors`, y a esa solo la llama
/// `embedded::engine_in`— y esto la fija desde fuera: mismo listado y mismo
/// cambiazo, y aquí la escritura SE HACE, porque sin ancla se está en 0.53.
#[tokio::test]
async fn el_engine_del_daemon_no_ancla_aunque_le_monten_un_backend_embebido() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(dir.path().join("d")).expect("destino");
    std::fs::create_dir(dir.path().join("d/sub")).expect("sub");
    std::fs::create_dir(dir.path().join("fuera")).expect("fuera");
    // Como lo construye el daemon: sin pasar por `embedded::engine_in`.
    let engine = Engine::new();
    assert!(
        !engine.has_client_anchors(),
        "un engine que no es de frontend no tiene memoria de anclas"
    );
    engine.register_provider(
        Arc::new(norte_vfs_local::LocalProvider::rooted(dir.path())) as Arc<dyn Provider>
    );
    let a = Arbol {
        backend: Backend::Embedded(Arc::new(engine)),
        dir,
    };

    listar_como_un_panel(&a.backend, "file:///d/sub").await;
    sustituir_por_enlace(&a);

    let task = a
        .backend
        .create_file(&vp("file:///d/sub/notas.txt"))
        .await
        .expect("encola");
    assert_eq!(task.join().await, TaskState::Completed);
    assert!(
        a.dir.path().join("fuera/notas.txt").exists(),
        "sin memoria de cliente no hay ancla, y sin ancla es el 0.53 de siempre"
    );
}

/// El directorio de verdad, listado y sin tocar: el ancla no cuesta la
/// operación. Sin este test, «rehúsa siempre» pasaría los tres de arriba.
#[tokio::test]
async fn el_destino_que_sigue_siendo_el_mismo_deja_crear() {
    let a = arbol();
    listar_como_un_panel(&a.backend, "file:///d/sub").await;

    let task = a
        .backend
        .create_file(&vp("file:///d/sub/notas.txt"))
        .await
        .expect("encola");
    assert_eq!(task.join().await, TaskState::Completed);
    assert!(a.dir.path().join("d/sub/notas.txt").exists());
}
