//! Integración `Engine::checksum_as` (#311): el digest del contenido de un
//! lote de ficheros, como Task cancelable, con los digests en un INFORME —
//! porque N sumas no caben en el desenlace de una Task ni en su progreso.
//!
//! `MemProvider` in-memory → determinista, sin tocar disco.

use std::sync::Arc;

use bytes::Bytes;
use norte_core::{Actor, Engine};
use norte_proto::methods::{ChecksumAlgo, ChecksumMiss, FsChecksumParams};
use norte_proto::{Error as ProtoError, TaskState, VPath};
use norte_testkit::MemProvider;
use norte_vfs::Provider;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("wire válido")
}

async fn write_file(mem: &MemProvider, wire: &str, contenido: &[u8]) {
    let mut sink = mem.write(&vp(wire)).await.expect("write abre");
    sink.write(Bytes::copy_from_slice(contenido))
        .await
        .expect("chunk");
    sink.commit().await.expect("commit");
}

fn setup() -> (Engine, Arc<MemProvider>) {
    let engine = Engine::new();
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn norte_vfs::Provider>);
    (engine, mem)
}

fn params(paths: &[&str]) -> FsChecksumParams {
    FsChecksumParams {
        paths: paths.iter().map(|p| vp(p)).collect(),
        algo: ChecksumAlgo::Sha256,
    }
}

/// El digest de un fichero vacío y el de uno con contenido, contra los valores
/// que publica cualquier `sha256sum`: si esto se desviara, comprobar contra una
/// suma de fuera dejaría de servir para nada — que es justo para lo que existe.
#[tokio::test]
async fn los_digests_son_los_de_sha256sum() {
    let (engine, mem) = setup();
    write_file(&mem, "mem:///vacio", b"").await;
    write_file(&mem, "mem:///abc", b"abc").await;

    let handle = engine
        .checksum_as(params(&["mem:///vacio", "mem:///abc"]), Actor::User)
        .await
        .expect("lanza");
    let id = handle.id();
    assert_eq!(handle.join().await, TaskState::Completed);

    let (_actor, informe) = engine.checksum_report(id).expect("hay informe");
    assert_eq!(informe.pending, 0, "terminada: no queda nada por resolver");
    assert_eq!(informe.entries.len(), 2);
    assert_eq!(
        informe.entries[0].digest.as_deref(),
        Some("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"),
        "el sha256 del fichero vacío"
    );
    assert_eq!(
        informe.entries[1].digest.as_deref(),
        Some("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"),
        "el sha256 de `abc`"
    );
}

/// El orden del informe es el de la PETICIÓN. Un informe que se reordenara solo
/// no se podría comparar con la lista que uno mandó.
#[tokio::test]
async fn el_informe_conserva_el_orden_pedido() {
    let (engine, mem) = setup();
    write_file(&mem, "mem:///a", b"a").await;
    write_file(&mem, "mem:///b", b"b").await;
    write_file(&mem, "mem:///c", b"c").await;

    let handle = engine
        .checksum_as(params(&["mem:///c", "mem:///a", "mem:///b"]), Actor::User)
        .await
        .expect("lanza");
    let id = handle.id();
    assert_eq!(handle.join().await, TaskState::Completed);
    let (_actor, informe) = engine.checksum_report(id).expect("informe");
    let rutas: Vec<String> = informe.entries.iter().map(|e| e.path.to_wire()).collect();
    assert_eq!(rutas, vec!["mem:///c", "mem:///a", "mem:///b"]);
}

/// Lo que no se pudo leer y lo que no era un fichero salen con su MOTIVO, y no
/// tumban el lote: comprobar cien ficheros no puede morirse en el que alguien
/// acaba de mover.
#[tokio::test]
async fn lo_ilegible_y_lo_que_no_es_fichero_salen_con_su_motivo() {
    let (engine, mem) = setup();
    write_file(&mem, "mem:///bueno", b"hola").await;
    mem.mkdir(&vp("mem:///carpeta")).await.expect("mkdir");

    let handle = engine
        .checksum_as(
            params(&["mem:///bueno", "mem:///carpeta", "mem:///no-existe"]),
            Actor::User,
        )
        .await
        .expect("lanza");
    let id = handle.id();
    assert_eq!(
        handle.join().await,
        TaskState::Completed,
        "un ilegible NO hace fallar la Task"
    );

    let (_actor, informe) = engine.checksum_report(id).expect("informe");
    assert_eq!(informe.entries.len(), 3, "las tres rutas salen");
    assert!(
        informe.entries[0].digest.is_some(),
        "el bueno sí tiene suma"
    );
    assert_eq!(informe.entries[1].miss, Some(ChecksumMiss::NotAFile));
    assert_eq!(informe.entries[2].miss, Some(ChecksumMiss::Unreadable));
    assert!(
        informe.entries[1].digest.is_none() && informe.entries[2].digest.is_none(),
        "sin suma cuando hay motivo: los dos campos son excluyentes"
    );
}

/// La lista vacía se rechaza ANTES de crear Task alguna: resumir la nada no es
/// una petición, y un rechazo del REQUEST no es el fallo de una Task lanzada.
#[tokio::test]
async fn sin_rutas_no_hay_task() {
    let (engine, _mem) = setup();
    let Err(err) = engine.checksum_as(params(&[]), Actor::User).await else {
        panic!("una lista vacía tiene que rechazarse");
    };
    assert!(matches!(err, ProtoError::InvalidPath), "{err:?}");
}

/// Por encima del tope se RECHAZA, no se recorta: un informe recortado en
/// silencio se lee como «todo comprobado» sobre ficheros que nadie miró.
#[tokio::test]
async fn por_encima_del_tope_se_rechaza() {
    let (engine, _mem) = setup();
    let muchas: Vec<VPath> = (0..=norte_proto::methods::FS_CHECKSUM_MAX_PATHS)
        .map(|i| vp(&format!("mem:///f{i}")))
        .collect();
    let Err(err) = engine
        .checksum_as(
            FsChecksumParams {
                paths: muchas,
                algo: ChecksumAlgo::Sha256,
            },
            Actor::User,
        )
        .await
    else {
        panic!("por encima del tope tiene que rechazarse");
    };
    assert!(matches!(err, ProtoError::InvalidPath), "{err:?}");
}

/// Regla 3: el lote se cancela limpiamente, el estado lo dice, y el informe
/// **se queda a medias diciéndolo**.
///
/// `pending > 0` con la Task ya terminal es la señal de que lo que hay no es
/// todo. Sin ella, un frontend que compare contra un fichero de sumas acusaría
/// —«no cuadra o falta»— a ficheros que nadie llegó a leer, que es el peor
/// error posible en la única herramienta cuyo trabajo es comprobar.
#[tokio::test]
async fn cancelar_deja_el_informe_marcado_como_incompleto() {
    let (engine, mem) = setup();
    let mut rutas = Vec::new();
    for i in 0..400 {
        let wire = format!("mem:///f{i}");
        write_file(&mem, &wire, b"contenido").await;
        rutas.push(vp(&wire));
    }
    let handle = engine
        .checksum_as(
            FsChecksumParams {
                paths: rutas,
                algo: ChecksumAlgo::Sha256,
            },
            Actor::User,
        )
        .await
        .expect("lanza");
    let id = handle.id();
    handle.cancel();
    assert_eq!(handle.join().await, TaskState::Cancelled);

    let (_actor, informe) = engine.checksum_report(id).expect("hay informe");
    assert!(
        informe.pending > 0,
        "cancelado a mitad: lo que falta tiene que seguir contándose, \
         no ponerse a cero como si el lote hubiera acabado"
    );
    assert!(
        informe.entries.len() < 400,
        "si estuvieran las 400 no se canceló nada y el test no prueba nada"
    );
}

/// El informe dice CON QUÉ se calculó. Se puede pedir sin haber mandado la
/// petición —`task.list` enseña las tasks de otros—, así que asumir sha256 por
/// omisión sería pintar digests de otra cosa el día que haya un segundo
/// algoritmo.
#[tokio::test]
async fn el_informe_nombra_su_algoritmo() {
    let (engine, mem) = setup();
    write_file(&mem, "mem:///x", b"abc").await;
    let handle = engine
        .checksum_as(params(&["mem:///x"]), Actor::User)
        .await
        .expect("lanza");
    let id = handle.id();
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(
        engine.checksum_report(id).expect("informe").1.algo,
        ChecksumAlgo::Sha256
    );
}

/// Un id que nunca fue un lote de sumas no tiene informe — y eso es lo que el
/// daemon convierte en `NotFound` para quien pregunta por el de otro.
#[tokio::test]
async fn un_id_ajeno_no_tiene_informe() {
    let (engine, _mem) = setup();
    assert!(
        engine
            .checksum_report(norte_proto::TaskId::new(4242))
            .is_none()
    );
}
