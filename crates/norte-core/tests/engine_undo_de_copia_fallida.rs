//! **Una copia que falló no puede dejar un deshacer que borre lo que TÚ
//! pusiste.**
//!
//! Viene de #369, que encontró un revisor mirando el arreglo de #362 (ADR
//! 0151). La cadena es corta y todo su daño está en el último eslabón:
//!
//! 1. la copia escribe por el descriptor de la raíz de destino, y el diario
//!    anota la ruta LÓGICA (`Mutation::Created(/destino/f0001)`);
//! 2. si esa carpeta se borró —en norte, un `rename` a la papelera—, los
//!    bytes van a la papelera mientras el diario sigue anotando `/destino/...`;
//! 3. desde ADR 0151 la tarea FALLA, que es lo correcto, y esas entradas se
//!    quedan ahí describiendo ficheros que no están en esas rutas;
//! 4. lo natural después de «la carpeta de destino ya no está» es volver a
//!    crearla y repetir la copia. Ahora esas rutas SÍ existen, y lo que
//!    tienen dentro es la copia buena;
//! 5. deshacer aquel lote fallido borra la copia buena.
//!
//! O sea: un borrado provocado por una operación que no ocurrió. La regla 4
//! del proyecto ya dice qué hacer con esto — una mutación tiene su reversa, o
//! se clasifica `Irreversible` CON SU MOTIVO— y aquí el motivo se escribe
//! solo: lo que se escribió se fue a un directorio que ya no está en esa ruta,
//! así que en esas rutas no hay nada que deshacer.
//!
//! Va contra el sistema de ficheros REAL, como `engine_destino_que_desaparece`
//! y por lo mismo: lo que hace posible el caso es un descriptor que sobrevive
//! a un `rename`, y `MemProvider` no tiene descriptores.

#![cfg(unix)]

use std::sync::Arc;

use norte_core::{Engine, Journal, SqliteJournal};
use norte_proto::{CollisionPolicy, TaskState, VPath};
use norte_vfs::Provider;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("wire válido")
}

/// Bastantes para que la copia siga viva cuando el test interviene.
const FICHEROS: usize = 4000;

/// Sondea un HECHO, no un plazo. Ver `engine_destino_que_desaparece`.
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

/// Deja una copia a medias con el destino borrado, y devuelve el diario.
///
/// Es el estado de partida de los dos tests: lo que queda después de que el
/// lector borre la carpeta de destino mientras se copiaba.
async fn copia_con_el_destino_borrado(
    dir: &std::path::Path,
) -> (Engine, Arc<SqliteJournal>, TaskState) {
    let origen = dir.join("origen");
    std::fs::create_dir(&origen).expect("origen");
    for i in 0..FICHEROS {
        std::fs::write(origen.join(format!("f{i:04}")), vec![b'x'; 1024]).expect("fichero");
    }

    let journal = Arc::new(SqliteJournal::new(
        Journal::open_in_memory().await.expect("journal open"),
    ));
    // `with_journal` y no `with_observer`: el segundo anota pero no abre la
    // puerta del deshacer, y este test necesita poder deshacer.
    let engine = Engine::with_journal(Arc::clone(&journal));
    engine.register_provider(
        Arc::new(norte_vfs_local::LocalProvider::rooted(dir)) as Arc<dyn Provider>
    );

    // Una mutación ANTES de la copia, que hace de corte para el deshacer:
    // `undo_after` exige que el corte NOMBRE una entrada que existe, y se
    // niega a un cero — que es lo que sale de un cursor rancio y seleccionaría
    // la historia entera de alguien.
    engine
        .mkdir(&vp("file:///marca"))
        .await
        .expect("marca")
        .join()
        .await;

    let handle = engine
        .copy_with_as(
            &vp("file:///origen"),
            &vp("file:///destino"),
            norte_core::TransferOptions {
                on_collision: CollisionPolicy::Fail,
                ..norte_core::TransferOptions::default()
            },
            norte_core::Actor::User,
        )
        .await
        .expect("encola");

    let destino = dir.join("destino");
    assert!(
        espera(|| std::fs::read_dir(&destino).is_ok_and(|d| d.count() > 0)).await,
        "la copia no llegó a empezar"
    );
    let cuando_borre = std::fs::read_dir(&destino).expect("destino").count();
    std::fs::rename(&destino, dir.join("papelera")).expect("a la papelera");
    assert!(
        cuando_borre < FICHEROS,
        "la copia ya había acabado al borrar ({cuando_borre} de {FICHEROS}): \
         este test no ha probado nada"
    );

    let estado = tokio::time::timeout(std::time::Duration::from_mins(1), handle.join())
        .await
        .expect("la tarea se quedó colgada");
    (engine, journal, estado)
}

/// **Lo que el diario anotó no está donde dice.**
///
/// Este test no es el daño, es la premisa: enseña que después del fallo
/// quedan entradas `created` apuntando a rutas vacías. El de abajo enseña lo
/// que eso cuesta.
#[tokio::test]
async fn una_copia_fallida_deja_entradas_que_apuntan_a_rutas_vacias() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (_engine, journal, estado) = copia_con_el_destino_borrado(dir.path()).await;
    assert!(
        matches!(estado, TaskState::Failed { .. }),
        "la tarea tiene que fallar (ADR 0151), fue {estado:?}"
    );

    let entradas = journal.journal().entries().await.expect("entries");
    let creados: Vec<&norte_core::JournalEntry> = entradas
        .iter()
        .filter(|e| e.op == "created" && e.path.starts_with(b"file:///destino/"))
        .collect();
    assert!(
        !creados.is_empty(),
        "sin entradas no hay nada que demostrar: la copia no llegó a anotar nada"
    );
    // Y ninguna de esas rutas tiene nada: los bytes están en la papelera.
    assert_eq!(
        std::fs::read_dir(dir.path().join("destino"))
            .ok()
            .map(Iterator::count),
        None,
        "la carpeta de destino no existe, y el diario dice que creó ficheros dentro"
    );
    // Lo honesto sería que esas entradas dijeran que no tienen vuelta.
    let con_reversa = creados.iter().filter(|e| e.reversal == "delete").count();
    assert_eq!(
        con_reversa, 0,
        "{con_reversa} entradas prometen un `delete` que borraría lo que haya \
         en esas rutas, y lo que la copia escribió no está ahí: tienen que \
         quedar clasificadas irreversibles, con su motivo (regla 4)"
    );
}

/// **Y deshacerlo no puede llevarse por delante la copia BUENA.**
///
/// Éste es el daño, y con el gesto realista: falla la copia, el lector vuelve
/// a crear la carpeta y repite. Ahora esas rutas tienen la copia buena
/// dentro. Deshacer el lote fallido no puede tocarla.
#[tokio::test]
async fn deshacer_la_copia_fallida_no_borra_lo_que_se_puso_despues() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (engine, journal, _estado) = copia_con_el_destino_borrado(dir.path()).await;

    // El lector vuelve a crear la carpeta y REPITE la copia, que es lo que
    // hace cualquiera al leer «la carpeta de destino ya no está». Se simula
    // poniendo un fichero en CADA ruta que el diario anotó.
    //
    // Que estén todas es lo que hace de esto el caso real y no uno de
    // laboratorio: el deshacer recorre de lo más nuevo a lo más viejo y se
    // PARA en la primera ruta que no encuentra. Con una sola recreada, se
    // para en la segunda y no toca nada — el fichero se salva por el orden de
    // un fallo, no porque nadie lo esté protegiendo. Con la copia repetida no
    // hay nada que lo pare.
    let destino = dir.path().join("destino");
    std::fs::create_dir(&destino).expect("la nueva");
    let entradas = journal.journal().entries().await.expect("entries");
    let creados: Vec<Vec<u8>> = entradas
        .iter()
        .filter(|e| e.op == "created" && e.path.starts_with(b"file:///destino/"))
        .map(|e| e.path.clone())
        .collect();
    assert!(!creados.is_empty(), "sin entradas no hay nada que deshacer");
    for p in &creados {
        let nombre = std::str::from_utf8(&p[b"file:///destino/".len()..]).expect("utf8");
        std::fs::write(
            destino.join(nombre),
            b"la copia BUENA, la de la segunda vez",
        )
        .expect("repetida");
    }
    let suyo =
        destino.join(std::str::from_utf8(&creados[0][b"file:///destino/".len()..]).expect("utf8"));

    // Y deshace aquel lote: el corte es la marca, o sea todo lo de la copia.
    let entradas = journal.journal().entries().await.expect("entries");
    let corte = entradas.first().expect("la marca está").seq;
    let (handle, informe) = engine.undo_after(corte, None).await.expect("undo");
    let _ = tokio::time::timeout(std::time::Duration::from_mins(1), handle.join())
        .await
        .expect("el undo se quedó colgado");
    let r = informe.lock().expect("lock").clone();

    assert!(
        suyo.exists(),
        "el deshacer de una copia que FALLÓ se ha llevado un fichero que esa \
         copia nunca escribió: lo puso el lector al repetirla. Informe: {r:?}"
    );
    // Y dicho al derecho: esas entradas no tienen vuelta, así que el deshacer
    // tiene que SALTARLAS diciéndolo, no ejecutarlas.
    assert_eq!(
        r.undone, 0,
        "no había nada que deshacer en esas rutas: {r:?}"
    );
    assert!(
        r.skipped_irreversible > 0,
        "y el lector tiene que enterarse de que se saltaron: {r:?}"
    );
}
