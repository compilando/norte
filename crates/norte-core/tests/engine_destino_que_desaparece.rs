//! **Qué pasa si el destino desaparece MIENTRAS se copia.**
//!
//! Lo contó un lector: puso a copiar una carpeta grande y, con la barra de
//! progreso corriendo, borró la carpeta de destino. Al reproducirlo resultó
//! ser peor de lo que él describió — la tarea no se quedaba parada, terminaba
//! diciendo **«completada»**, y los ficheros estaban en la papelera.
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
//!
//! **No hace falta que el borrado venga de norte**, y eso es lo que decide el
//! diseño: se puede borrar la carpeta desde otro gestor, con un `rm` en una
//! terminal, o desde otra máquina sobre el mismo montaje. Negarse a borrarla
//! dentro de norte no cerraría nada; detectarlo y decirlo, sí.
//!
//! Los dos tests de aquí atacan los dos caminos por los que se detecta, y
//! hacen falta los dos: uno deja la ruta VACÍA (no resuelve a nada) y el otro
//! deja OTRO directorio en su sitio, que es el único caso donde la
//! comparación de identidad es lo que salva.

#![cfg(unix)]

use std::sync::Arc;

use norte_core::{Actor, Engine};
use norte_proto::{CollisionPolicy, ConflictKind, Error, TaskState, VPath};
use norte_vfs::Provider;

mod origen_a_peticion;
use origen_a_peticion::{Mando, OrigenAPeticion};

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("wire válido")
}

/// Cuántos ficheros lleva el origen.
///
/// Bastantes para que la copia siga viva cuando el test interviene. No es un
/// plazo disfrazado: el test no espera un tiempo, espera a VER que la copia
/// empezó, y con cuatro mil ficheros queda trabajo detrás de ese momento.
///
/// Son de 1 KiB porque lo que se prueba se dispara por ENTRADA, no por byte:
/// hacerlos grandes solo encarece el montaje del test.
const FICHEROS: usize = 4000;

/// Espera a que `cond` se cumpla, o se rinde.
///
/// Sondea en vez de dormir un rato fijo, que es lo que este repositorio pide:
/// lo que se espera es un HECHO observable —que haya aterrizado el primer
/// fichero—, no que pase un tiempo.
///
/// Treinta segundos y no diez: por debajo de este plazo hay el encolado, el
/// plan y la hidratación, que hace un `stat` por entrada de una en una. Con la
/// máquina cargada por el resto de la suite, diez era el número más apretado
/// del fichero y el primero que se habría puesto rojo sin motivo.
async fn espera(mut cond: impl FnMut() -> bool) -> bool {
    let hasta = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    while tokio::time::Instant::now() < hasta {
        if cond() {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    false
}

/// El montaje común: un origen con muchos ficheros y un motor que lo sirve.
fn arbol() -> (tempfile::TempDir, Engine) {
    let dir = tempfile::tempdir().expect("tempdir");
    let origen = dir.path().join("origen");
    std::fs::create_dir(&origen).expect("origen");
    for i in 0..FICHEROS {
        std::fs::write(origen.join(format!("f{i:04}")), vec![b'x'; 1024]).expect("fichero");
    }
    let engine = Engine::new();
    engine.register_provider(
        Arc::new(norte_vfs_local::LocalProvider::rooted(dir.path())) as Arc<dyn Provider>
    );
    (dir, engine)
}

/// Lanza la copia y espera a que esté DE VERDAD copiando.
///
/// Devuelve el handle y cuántos ficheros había en el destino en ese momento,
/// que es lo que luego demuestra que el test llegó a tiempo.
async fn copiando(dir: &std::path::Path, engine: &Engine) -> (norte_core::TaskHandle, usize) {
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

    // Se espera a que haya ALGO dentro, no a un nombre concreto. Esperar a
    // `f0000` fue uno de los intentos fallidos: el árbol se recorre en el
    // orden que da `readdir`, que no es alfabético, así que ese fichero podía
    // ser el último — y el test intervenía con la copia ya acabada, creyendo
    // que la pillaba empezando.
    let destino = dir.join("destino");
    assert!(
        espera(|| std::fs::read_dir(&destino).is_ok_and(|d| d.count() > 0)).await,
        "la copia no llegó a empezar"
    );
    // Y se mide JUSTO ANTES de intervenir. Contarlo después no vale, y ése fue
    // el primer intento fallido: tras el `rename` la copia sigue llenando esa
    // misma carpeta por el descriptor, así que el contador acababa completo
    // dijera lo que dijera la realidad en el instante que importa.
    let cuantos = std::fs::read_dir(&destino).expect("destino").count();
    (handle, cuantos)
}

/// Espera el desenlace SIN poder colgarse.
///
/// El síntoma que el lector describió —una tarea que se queda ahí— es
/// precisamente el que un `join()` pelado convertiría en un test colgado en
/// vez de en un test rojo: saltaría el plazo de nextest minutos después, con
/// un informe de timeout en lugar del mensaje que aquí está escrito.
async fn desenlace(handle: norte_core::TaskHandle) -> TaskState {
    tokio::time::timeout(std::time::Duration::from_mins(1), handle.join())
        .await
        .expect("la tarea se quedó colgada: ni completa, ni falla, ni se cancela")
}

fn a_tiempo(cuantos: usize) {
    assert!(
        cuantos < FICHEROS,
        "la copia ya había acabado al intervenir ({cuantos} de {FICHEROS}): \
         este test no ha probado nada, hace falta un origen más grande"
    );
}

/// **Borrar el destino a media copia no se salda como completada.**
///
/// Se borra como lo borra norte: a la papelera, que es un `rename`. Lo que la
/// copia tiene abierto es el descriptor de esa carpeta, así que después del
/// `rename` sigue escribiendo dentro — en la papelera, donde el lector no
/// puso nada y no va a mirar.
///
/// Aquí la ruta queda VACÍA, así que lo que detecta el caso es que la ruta no
/// resuelve. El test de abajo cubre el otro camino.
#[tokio::test]
async fn borrar_el_destino_a_media_copia_no_se_salda_como_completada() {
    let (dir, engine) = arbol();
    let (handle, cuantos) = copiando(dir.path(), &engine).await;

    // Entre la cuenta de arriba y este `rename` la copia no puede avanzar:
    // este test corre en un runtime de UN hilo, y mientras el cuerpo del test
    // hace E/S síncrona lo tiene cogido. Es load-bearing y no se ve — el día
    // que alguien le ponga `flavor = "multi_thread"` para acelerarlo, esa
    // ventana se abre y el test puede fallar con el mensaje equivocado.
    let papelera = dir.path().join("papelera");
    std::fs::rename(dir.path().join("destino"), &papelera).expect("a la papelera");
    a_tiempo(cuantos);

    let estado = desenlace(handle).await;
    assert_ne!(
        estado,
        TaskState::Completed,
        "el destino dejó de existir a media copia y la tarea dice que copió: \
         lo copiado está en {}, que no es donde se pidió",
        papelera.display()
    );
    // Y no vale con «falló»: tiene que fallar DICIENDO QUÉ. Antes contestaba
    // `NotFound` a secas, que en mitad de una copia de miles de ficheros se
    // lee como «falta algo del ORIGEN» — lo contrario de lo que pasó.
    assert!(
        matches!(
            estado,
            TaskState::Failed {
                error: Error::Conflict {
                    conflict: ConflictKind::DestinationGone
                }
            }
        ),
        "tiene que decir que el destino se fue, no {estado:?}"
    );
}

/// **Y si en su sitio aparece OTRA carpeta, tampoco.**
///
/// Éste es el caso que de verdad prueba la comparación de identidad, y por eso
/// hace falta además del de arriba: aquí la ruta SÍ resuelve —hay un
/// directorio en `file:///destino`— así que «no encuentro la ruta» no salva a
/// nadie. Lo único que distingue este destino del bueno es que su inodo no es
/// el del descriptor que la copia tiene abierto.
///
/// Sin este test, la comprobación se podría reducir a un `stat` de la ruta y
/// todo seguiría verde, mientras la copia sigue llenando el directorio viejo.
///
/// Y es el caso REALISTA, no el rebuscado: borrar la carpeta y volver a
/// crearla es exactamente lo que hace alguien que quería empezar de cero.
#[tokio::test]
async fn si_en_el_sitio_del_destino_aparece_otra_carpeta_la_copia_para() {
    let (dir, engine) = arbol();
    let (handle, cuantos) = copiando(dir.path(), &engine).await;

    let destino = dir.path().join("destino");
    std::fs::rename(&destino, dir.path().join("papelera")).expect("a la papelera");
    // Y el lector crea una carpeta nueva con el mismo nombre.
    std::fs::create_dir(&destino).expect("la nueva");
    a_tiempo(cuantos);

    let estado = desenlace(handle).await;
    assert!(
        matches!(
            estado,
            TaskState::Failed {
                error: Error::Conflict {
                    conflict: ConflictKind::DestinationGone
                }
            }
        ),
        "la ruta resuelve, pero a OTRO directorio: la copia no puede seguir \
         llenando el viejo y decir que fue bien. Fue {estado:?}"
    );
    // Y lo que el lector ve en su carpeta nueva es lo que él puso: nada.
    assert_eq!(
        std::fs::read_dir(&destino).expect("la nueva").count(),
        0,
        "no se escribió ni un fichero en la carpeta nueva"
    );
}

/// El montaje del fichero suelto: un origen que se puede parar, un destino
/// local de verdad, y el motor que los une.
async fn un_fichero_parado(
    dir: &std::path::Path,
) -> (Engine, Arc<norte_testkit::MemProvider>, Mando) {
    let mem = Arc::new(norte_testkit::MemProvider::new());
    // Varios trozos por parecerse a un fichero de verdad; la parada no
    // depende de que haya más de uno (ver `OrigenAPeticion::read`).
    {
        let mut sink = mem.write(&vp("lento:///grande")).await.expect("write");
        for _ in 0..8 {
            sink.write(bytes::Bytes::from(vec![b'x'; 64 * 1024]))
                .await
                .expect("chunk");
        }
        sink.commit().await.expect("commit");
    }
    let (origen, mando) = OrigenAPeticion::nuevo(Arc::clone(&mem));
    let engine = Engine::new();
    engine.register_provider(
        Arc::new(norte_vfs_local::LocalProvider::rooted(dir)) as Arc<dyn Provider>
    );
    engine.register_provider(origen);
    (engine, mem, mando)
}

/// **Y MOVER un fichero es el caso que pierde datos.**
///
/// Lo encontró la revisión de #367 mirando el arreglo, y es peor que lo que
/// #367 cerraba: un movimiento entre providers copia la hoja y después BORRA
/// el origen. Con la carpeta de destino borrada a media copia, el resultado
/// era los bytes en la papelera, el origen destruido y la tarea diciendo
/// «completada». Aquí la comprobación tiene que ir antes del borrado, y no
/// antes de la frase final: lo que hay detrás es un efecto irreversible.
#[tokio::test]
async fn mover_un_fichero_a_un_destino_que_desaparece_no_borra_el_origen() {
    let dir = tempfile::tempdir().expect("tempdir");
    let destino = dir.path().join("destino");
    std::fs::create_dir(&destino).expect("destino");
    let (engine, mem, mut mando) = un_fichero_parado(dir.path()).await;

    let handle = engine
        .move_with_as(
            &vp("lento:///grande"),
            &vp("file:///destino/grande"),
            norte_core::TransferOptions {
                on_collision: CollisionPolicy::Fail,
                ..norte_core::TransferOptions::default()
            },
            Actor::User,
        )
        .await
        .expect("encola");

    assert!(
        mando.empezo().await,
        "el movimiento no llegó a empezar: este test no ha probado nada"
    );
    std::fs::rename(&destino, dir.path().join("papelera")).expect("a la papelera");
    mando.sigue();

    let estado = desenlace(handle).await;
    assert!(
        matches!(
            estado,
            TaskState::Failed {
                error: Error::Conflict {
                    conflict: ConflictKind::DestinationGone
                }
            }
        ),
        "un movimiento cuyo destino se fue no puede acabar «completada». Fue {estado:?}"
    );
    // Y esto es el daño de verdad, no la frase: el origen sigue ahí.
    assert!(
        mem.stat(&vp("lento:///grande")).await.is_ok(),
        "el movimiento borró el origen después de copiarlo a una carpeta que \
         ya no estaba: los bytes en la papelera y el fichero destruido"
    );
}

/// **#367 — copiar UN fichero tiene el mismo agujero.**
///
/// `open_leaf_root` comprueba la raíz UNA vez, al abrirla, que es la forma que
/// tenía el árbol antes de ADR 0151. A partir de ahí la hoja se escribe y se
/// publica por ese descriptor, así que borrar la carpeta de destino con la
/// copia en marcha dejaba el fichero en la papelera y la tarea diciendo que
/// fue bien.
#[tokio::test]
async fn copiar_un_fichero_a_un_destino_que_desaparece_falla() {
    let dir = tempfile::tempdir().expect("tempdir");
    let destino = dir.path().join("destino");
    std::fs::create_dir(&destino).expect("destino");

    let (engine, _mem, mut mando) = un_fichero_parado(dir.path()).await;

    let handle = engine
        .copy_with_as(
            &vp("lento:///grande"),
            &vp("file:///destino/grande"),
            norte_core::TransferOptions {
                on_collision: CollisionPolicy::Fail,
                ..norte_core::TransferOptions::default()
            },
            Actor::User,
        )
        .await
        .expect("encola");

    // El hecho, no un plazo: la copia ya entregó su primer trozo.
    assert!(
        mando.empezo().await,
        "la copia no llegó a empezar: este test no ha probado nada"
    );
    // Con la copia PARADA a mitad, el lector borra la carpeta de destino.
    std::fs::rename(&destino, dir.path().join("papelera")).expect("a la papelera");
    mando.sigue();

    let estado = desenlace(handle).await;
    assert!(
        matches!(
            estado,
            TaskState::Failed {
                error: Error::Conflict {
                    conflict: ConflictKind::DestinationGone
                }
            }
        ),
        "un fichero suelto cuyo destino se fue no puede acabar «completada»: \
         los bytes están en la papelera. Fue {estado:?}"
    );
}
