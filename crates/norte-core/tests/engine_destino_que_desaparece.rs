//! **Qué pasa si el destino desaparece MIENTRAS se copia.**
//!
//! Lo contó un lector: puso a copiar una carpeta grande, y con la barra de
//! progreso corriendo borró la carpeta de destino. Se borró. Y la tarea se
//! quedó ahí, diciendo que copiaba, sin que pasara nada.
//!
//! Va contra el sistema de ficheros REAL y no contra `MemProvider`, por lo
//! mismo que `engine_leaf_confined`: lo que hace posible el fallo es que la
//! copia direcciona por un DESCRIPTOR de la raíz de destino, abierto una vez
//! (#164), y un descriptor no es una ruta. `MemProvider` no tiene descriptores
//! con los que reproducirlo.
//!
//! Y el borrado de norte es a la papelera, o sea un `rename`
//! (`trash_fdo::do_rename`). Ahí está la diferencia que lo convierte en un
//! fallo silencioso en vez de en un error: un `rename` **no invalida** el
//! descriptor. El directorio sigue existiendo, con su mismo inodo, en otro
//! sitio — y la copia sigue llenándolo, donde nadie lo va a buscar.

#![cfg(unix)]

use std::sync::Arc;

use norte_core::{Actor, Engine};
use norte_proto::{CollisionPolicy, TaskState, VPath};
use norte_vfs::Provider;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("wire válido")
}

/// Cuántos ficheros lleva el origen.
///
/// Bastantes para que la copia siga viva cuando el test interviene. No es un
/// plazo disfrazado: el test no espera un tiempo, espera a VER que la copia
/// empezó, y con cuatrocientos ficheros queda trabajo detrás de ese momento.
const FICHEROS: usize = 4000;

/// Espera a que `cond` se cumpla, o se rinde.
///
/// Sondea en vez de dormir un rato fijo, que es lo que este repositorio pide:
/// lo que se espera es un HECHO observable —que el primer fichero haya
/// aterrizado—, no que pase un tiempo.
async fn espera(mut cond: impl FnMut() -> bool) -> bool {
    let hasta = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    while tokio::time::Instant::now() < hasta {
        if cond() {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    false
}

/// **Borrar el destino a media copia no puede dejar la copia escribiendo a
/// ciegas.**
///
/// Se borra como lo borra norte: a la papelera, que es un `rename`. Lo que la
/// copia tiene abierto es el descriptor de esa carpeta, así que después del
/// `rename` sigue escribiendo dentro — en la papelera, donde el lector no
/// puso nada y no va a mirar.
///
/// Lo que se exige es lo mínimo honesto: que la tarea NO diga que completó.
/// Si el sitio al que el lector mandó la copia ya no es ese directorio, la
/// copia no ha hecho lo que le pidieron, y decir «hecho» es la peor de las
/// respuestas posibles — peor que fallar, porque nadie va a comprobarlo.
#[tokio::test]
async fn borrar_el_destino_a_media_copia_no_se_salda_como_completada() {
    let dir = tempfile::tempdir().expect("tempdir");
    let origen = dir.path().join("origen");
    std::fs::create_dir(&origen).expect("origen");
    for i in 0..FICHEROS {
        std::fs::write(origen.join(format!("f{i:04}")), vec![b'x'; 4 * 1024]).expect("fichero");
    }

    let engine = Engine::new();
    engine.register_provider(
        Arc::new(norte_vfs_local::LocalProvider::rooted(dir.path())) as Arc<dyn Provider>
    );
    let handle = engine
        .copy_with_as(
            &vp("file:///origen"),
            &vp("file:///destino"),
            norte_core::TransferOptions {
                on_collision: CollisionPolicy::Fail,
                ..norte_core::TransferOptions::default()
            },
            Actor::User,
        )
        .await
        .expect("encola");

    // Se interviene cuando la copia ESTÁ pasando, no antes: el primer fichero
    // dentro del destino es la prueba de que la raíz ya está abierta.
    //
    // Se espera a que haya ALGO dentro, no a un nombre concreto. Esperar a
    // `f0000` fue el segundo intento fallido: el árbol se recorre en el orden
    // que da `readdir`, que no es alfabético, así que ese fichero podía ser el
    // último — y el test intervenía con la copia ya acabada, creyendo que la
    // pillaba empezando.
    let destino = dir.path().join("destino");
    assert!(
        espera(|| std::fs::read_dir(&destino).is_ok_and(|d| d.count() > 0)).await,
        "la copia no llegó a empezar"
    );

    // Que el test llegue A TIEMPO no se supone: se mide, y se mide JUSTO
    // ANTES de borrar. Contarlo después no vale, y ese fue el primer intento:
    // tras el `rename` la copia sigue llenando esa misma carpeta por el
    // descriptor, así que el contador acababa en 4000 dijera lo que dijera la
    // realidad en el instante que importa.
    let cuando_borre = std::fs::read_dir(&destino).expect("destino").count();

    // Y aquí el lector borra la carpeta de destino.
    let papelera = dir.path().join("papelera");
    std::fs::rename(&destino, &papelera).expect("a la papelera");

    assert!(
        cuando_borre < FICHEROS,
        "la copia ya había acabado al borrar ({cuando_borre} de {FICHEROS}): \
         este test no ha probado nada, hace falta un origen más grande"
    );

    let estado = handle.join().await;
    assert_ne!(
        estado,
        TaskState::Completed,
        "el destino dejó de existir a media copia y la tarea dice que copió: \
         lo copiado está en {}, que no es donde se pidió",
        papelera.display()
    );
    // Y no vale con «no completada»: tiene que FALLAR, con su motivo. Una
    // tarea que se queda en cualquier otro estado es la mitad del fallo que
    // esto arregla — el lector la ve ahí parada y no sabe si sigue o no.
    assert!(
        matches!(estado, TaskState::Failed { .. }),
        "tiene que fallar y decirlo, no quedarse en {estado:?}"
    );
}
