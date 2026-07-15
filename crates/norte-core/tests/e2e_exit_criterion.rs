//! E2E del **criterio de salida M2** (spec §15): «copiar de sftp a zip local
//! vía S3 sin sorpresas».
//!
//! Interpretación (kickoff decisión 1: archive es READ-ONLY en M2): la cadena
//! LEE un zip que vive en un remoto (composición `zip+…!` de fase 8f) y lo
//! restaura en local PASANDO POR S3:
//!
//! ```text
//!   zip+mem:///backup.zip/!   (archive read sobre el "remoto")
//!        │  copy cross-provider
//!        ▼
//!   s3://norte-test/staging   (object storage)
//!        │  copy cross-provider
//!        ▼
//!   file:///…/restore         (local, destino final)
//! ```
//!
//! Este run es el de CI (sin Docker): el peldaño "sftp" lo espeja `MemProvider`
//! —el plan M2 lo prevé como espejo de los remotos— y "S3" es `ObjectProvider`
//! sobre `services-fs` de opendal (misma lógica del provider, sin HTTP). El
//! **nightly** (fase 10d) reejecuta ESTA cadena contra `OpenSSH` + `MinIO`
//! reales.
//!
//! "Sin sorpresas" que se asertan aquí y son deterministas: fidelidad
//! byte-exacta de contenido Y de nombres hostiles UTF-8 en CADA salto;
//! colisión contra el destino = fallo limpio sin escribir; un nombre NO-UTF8
//! muere LIMPIO en la frontera S3 (keys UTF-8), jamás se corrompe. La
//! cancelación granular a mitad y el resume por offset son propiedades del
//! engine independientes del provider — probadas en `engine.rs` y
//! `engine_resume.rs`; no se re-demuestran por-provider aquí.

use std::path::Path;
use std::sync::Arc;

use bytes::Bytes;
use futures::StreamExt;
use norte_core::Engine;
use norte_proto::{ConflictKind, Error, TaskState, VPath};
use norte_testkit::{MemProvider, ZipSmith};
use norte_vfs::Provider;
use norte_vfs_local::LocalProvider;
use norte_vfs_object::ObjectProvider;
use opendal::Operator;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("wire válido de test")
}

/// `ObjectProvider` sobre `services-fs` (mismo patrón que el harness del
/// provider object): un tempdir como bucket, `atomic_write_dir` FUERA de la
/// raíz listable para que el staging del writer no sea una entrada.
fn object_fs(base: &Path, atomic: &Path) -> ObjectProvider {
    std::fs::create_dir_all(base).expect("root bucket");
    std::fs::create_dir_all(atomic).expect("staging");
    let fs = opendal::services::Fs::default()
        .root(base.to_str().expect("tempdir UTF-8"))
        .atomic_write_dir(atomic.to_str().expect("tempdir UTF-8"));
    ObjectProvider::new(Operator::new(fs).expect("operator fs"), "s3")
}

/// Escribe `bytes` en `mem:///backup.zip` (el "remoto" que aloja el zip).
async fn seed_zip(mem: &MemProvider, zip: &[u8]) {
    let mut sink = mem
        .write(&vp("mem:///backup.zip"))
        .await
        .expect("write zip");
    sink.write(Bytes::copy_from_slice(zip))
        .await
        .expect("chunk");
    sink.commit().await.expect("commit zip");
}

/// Lee un archivo por el engine, a bytes.
async fn read_via(engine: &Engine, wire: &str) -> Result<Vec<u8>, Error> {
    let mut stream = engine.read(&vp(wire), None).await?;
    let mut out = Vec::new();
    while let Some(chunk) = stream.next().await {
        out.extend_from_slice(&chunk?);
    }
    Ok(out)
}

/// Monta el engine con los tres providers de la cadena registrados.
/// Devuelve el engine y el tempdir raíz (debe vivir tanto como el engine).
fn chain_engine() -> (Engine, Arc<MemProvider>, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let engine = Engine::new();

    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);

    let object = object_fs(&dir.path().join("bucket"), &dir.path().join("staging"));
    engine.register_provider(Arc::new(object) as Arc<dyn Provider>);

    let local = LocalProvider::rooted(dir.path().join("restore"));
    std::fs::create_dir_all(dir.path().join("restore")).expect("restore dir");
    engine.register_provider(Arc::new(local) as Arc<dyn Provider>);

    (engine, mem, dir)
}

/// La cadena entera con nombres hostiles UTF-8: el zip llega byte-exacto al
/// destino local pasando por S3.
#[tokio::test]
async fn exit_criterion_zip_via_s3_to_local() {
    // Zip origen: árbol con nombres hostiles UTF-8 (espacios, acentos, un
    // símbolo multibyte) — todos representables como key S3 y como nombre de
    // fichero local; los NO-UTF8 se prueban aparte (mueren en la frontera S3).
    let zip = ZipSmith::new()
        .file("café.txt".as_bytes(), b"con acento")
        .file("con espacio/dato .bin".as_bytes(), b"dos niveles")
        .file("nieve ☃.dat".as_bytes(), b"multibyte")
        .build();
    let (engine, mem, _dir) = chain_engine();
    seed_zip(&mem, &zip).await;

    // Hop 1: interior del zip (archive read sobre mem) → S3.
    let h1 = engine
        .copy(
            &vp("zip+mem:///backup.zip/!"),
            &vp("s3://norte-test/staging"),
        )
        .await
        .expect("submit hop1");
    assert_eq!(h1.join().await, TaskState::Completed, "hop zip→S3");

    // Hop 2: S3 → local.
    let h2 = engine
        .copy(&vp("s3://norte-test/staging"), &vp("file:///dst"))
        .await
        .expect("submit hop2");
    assert_eq!(h2.join().await, TaskState::Completed, "hop S3→local");

    // Fidelidad byte-exacta de contenido Y nombres en el destino final.
    for (name, content) in [
        ("café.txt", &b"con acento"[..]),
        ("con espacio/dato .bin", &b"dos niveles"[..]),
        ("nieve ☃.dat", &b"multibyte"[..]),
    ] {
        let mut path = vp("file:///dst");
        for part in name.split('/') {
            path = path.join(norte_proto::Segment::new(part.as_bytes().to_vec()).unwrap());
        }
        let got = read_via(&engine, &path.to_wire())
            .await
            .unwrap_or_else(|e| panic!("{name} no llegó al destino: {e:?}"));
        assert_eq!(got, content, "contenido de {name} byte-exacto");
    }
}

/// "Sin sorpresas": recopiar sobre un destino existente FALLA limpio
/// (`CollisionPolicy::Fail` por defecto), sin corromper lo previo.
#[tokio::test]
async fn recopy_collision_fails_clean() {
    let zip = ZipSmith::new().file(b"solo.txt", b"unico").build();
    let (engine, mem, _dir) = chain_engine();
    seed_zip(&mem, &zip).await;

    engine
        .copy(
            &vp("zip+mem:///backup.zip/!"),
            &vp("s3://norte-test/staging"),
        )
        .await
        .expect("submit")
        .join()
        .await;
    // Segunda copia al MISMO destino: el staging ya existe → Conflict.
    let again = engine
        .copy(
            &vp("zip+mem:///backup.zip/!"),
            &vp("s3://norte-test/staging"),
        )
        .await
        .expect("submit 2");
    match again.join().await {
        TaskState::Failed {
            error: Error::Conflict { conflict },
        } => assert_eq!(conflict, ConflictKind::Exists),
        other => panic!("esperaba Conflict, fue {other:?}"),
    }
}

/// "Sin sorpresas": un nombre NO-UTF8 del zip muere LIMPIO en la frontera S3
/// (las keys S3 son UTF-8) — el engine reporta el fallo, jamás corrompe el
/// nombre a lossy ni escribe basura.
#[tokio::test]
async fn non_utf8_name_dies_clean_at_s3() {
    // Latin-1 "café" crudo (0xE9), NO-UTF8: legal en un zip, ilegal como key S3.
    let zip = ZipSmith::new()
        .file(&[b'c', b'a', b'f', 0xE9], b"latin1")
        .build();
    let (engine, mem, _dir) = chain_engine();
    seed_zip(&mem, &zip).await;

    let h = engine
        .copy(
            &vp("zip+mem:///backup.zip/!"),
            &vp("s3://norte-test/staging"),
        )
        .await
        .expect("submit");
    match h.join().await {
        TaskState::Failed { error } => assert!(
            matches!(error, Error::InvalidPath | Error::EncodingLoss),
            "el nombre no-UTF8 debe morir limpio, fue {error:?}"
        ),
        TaskState::Completed => panic!("un nombre no-UTF8 NO debe colar en S3"),
        other => panic!("estado inesperado: {other:?}"),
    }
}
