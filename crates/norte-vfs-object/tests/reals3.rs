//! NIGHTLY: provider object contra un servidor S3 REAL (`MinIO`) por
//! testcontainers (ADR 0016 J, spec §12). Fuera del gate de PR (exige
//! Docker): lo corre `just it-remote` desde el workflow nightly.
//!
//! Cubre lo que el harness in-process NO da (issue #50): semántica de dirs
//! (markers/prefix-probe, que s3s-fs rompe), keys largas (>`NAME_MAX` del FS
//! host), `copy_native` con `If-None-Match` real (s3s-fs lo ignora) y el
//! conditional write que `MinIO` SÍ valida.
#![cfg(feature = "it-s3")]

use bytes::Bytes;
use futures::TryStreamExt;
use norte_proto::{Authority, ConflictKind, EntryKind, Error, Segment, VPath};
use norte_vfs::Provider;
use norte_vfs_object::{ObjectProvider, Operator};
use testcontainers::core::WaitFor;
use testcontainers::runners::AsyncRunner;
use testcontainers::{GenericImage, ImageExt};

const AK: &str = "norteadmin";
const SK: &str = "nortesecret";
const BUCKET: &str = "norte-test";

fn root() -> VPath {
    ObjectProvider::root("s3", Authority::new(BUCKET).expect("authority"))
}

fn child(base: &VPath, name: &[u8]) -> VPath {
    base.join(Segment::new(name.to_vec()).expect("segmento"))
}

async fn read_all(p: &ObjectProvider, f: &VPath) -> Vec<u8> {
    let mut s = p.read(f, None).await.expect("read");
    let mut out = Vec::new();
    while let Some(chunk) = s.try_next().await.expect("chunk") {
        out.extend_from_slice(&chunk);
    }
    out
}

async fn write_all(p: &ObjectProvider, f: &VPath, data: &[u8]) {
    let mut sink = p.write(f).await.expect("write");
    sink.write(Bytes::copy_from_slice(data))
        .await
        .expect("chunk");
    sink.commit().await.expect("commit");
}

/// Un contenedor `MinIO` (bitnami: auto-crea el bucket vía `MINIO_DEFAULT_BUCKETS`)
/// más el provider y el `Operator` crudo. El Operator siembra keys "desde
/// fuera" (como otra herramienta): un prefijo sin marker que el propio provider
/// no crearía por su check de padre-existe. Un solo test por contenedor.
async fn setup() -> (
    testcontainers::ContainerAsync<GenericImage>,
    ObjectProvider,
    Operator,
) {
    // bitnamilegacy: bitnami movió sus imágenes públicas a este namespace en
    // 2025 (auto-crea el bucket con MINIO_DEFAULT_BUCKETS, lo que la imagen
    // oficial minio/minio no soporta). El WaitFor es laxo: setup() reintenta
    // el list hasta que el bucket exista.
    let container = GenericImage::new("bitnamilegacy/minio", "latest")
        // El banner de MinIO va a STDERR (el setup de bitnami a stdout).
        .with_wait_for(WaitFor::message_on_stderr("MinIO Object Storage Server"))
        .with_env_var("MINIO_ROOT_USER", AK)
        .with_env_var("MINIO_ROOT_PASSWORD", SK)
        .with_env_var("MINIO_DEFAULT_BUCKETS", BUCKET)
        .start()
        .await
        .expect("arrancar MinIO");
    let port = container.get_host_port_ipv4(9000).await.expect("puerto");
    opendal::install_default();
    let builder = opendal::services::S3::default()
        .bucket(BUCKET)
        .region("us-east-1")
        .endpoint(&format!("http://127.0.0.1:{port}"))
        .access_key_id(AK)
        .secret_access_key(SK)
        .disable_config_load()
        .disable_ec2_metadata();
    // El bucket puede tardar un instante en existir tras el arranque: reintenta.
    let op = Operator::new(builder).expect("operator");
    for _ in 0..40 {
        if op.list_with("").limit(1).await.is_ok() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    (container, ObjectProvider::new(op.clone(), "s3"), op)
}

/// Semántica de dirs contra S3 real: mkdir (marker) + stat, prefijo sin marker
/// = Dir, `remove` de dir no vacío = Conflict, `rename` de subárbol.
#[tokio::test]
async fn dirs_markers_y_rename() {
    let (_c, p, op) = setup().await;
    let r = root();
    // mkdir + stat del marker (dir VACÍO: el caso que s3s-fs no da bien).
    let d = child(&r, b"undir");
    p.mkdir(&d).await.expect("mkdir");
    assert_eq!(p.stat(&d).await.expect("stat dir").kind, EntryKind::Dir);
    // Fichero dentro; dir no vacío no se borra.
    write_all(&p, &child(&d, b"f.txt"), b"x").await;
    assert!(matches!(
        p.remove(&d).await,
        Err(Error::Conflict {
            conflict: ConflictKind::TypeMismatch
        })
    ));
    // Prefijo SIN marker (sembrado por el Operator crudo, como otra
    // herramienta): el provider no lo crearía por su check de padre-existe.
    op.write("prefijo/hijo.txt", b"y".to_vec())
        .await
        .expect("seed prefijo");
    assert_eq!(
        p.stat(&child(&r, b"prefijo"))
            .await
            .expect("stat prefijo")
            .kind,
        EntryKind::Dir
    );
    // rename del subárbol: contenido byte-exacto en destino, origen desaparecido.
    let dst = child(&r, b"movido");
    p.rename(&d, &dst).await.expect("rename dir");
    assert_eq!(read_all(&p, &child(&dst, b"f.txt")).await, b"x");
    assert_eq!(
        p.stat(&child(&d, b"f.txt")).await.unwrap_err(),
        Error::NotFound
    );
}

/// Keys largas que el harness fs in-process (`NAME_MAX` del host + el `.XXXXXXXX`
/// del `atomic_write_dir` = tope 246) no cubre. `MinIO` (backend de FS) limita cada
/// COMPONENTE a 255 bytes como un `NAME_MAX` real — así que se prueba 250
/// (>246 del harness, ≤255 de `MinIO`). AWS real acepta hasta 1024 en la key
/// completa (ADR 0016 D); esa cota total solo la valida AWS, no `MinIO`.
#[tokio::test]
async fn keys_largas_byte_exactas() {
    let (_c, p, _op) = setup().await;
    let r = root();
    let nombre_largo = "x".repeat(250);
    let f = child(&r, nombre_largo.as_bytes());
    write_all(&p, &f, b"contenido").await;
    assert_eq!(read_all(&p, &f).await, b"contenido");
    // Aparece byte-exacto en el listado.
    let listed: Vec<Vec<u8>> = p
        .list(&r)
        .await
        .expect("list")
        .try_collect::<Vec<_>>()
        .await
        .expect("stream")
        .into_iter()
        .map(|e| e.path.file_name().expect("nombre").as_bytes().to_vec())
        .collect();
    assert!(listed.contains(&nombre_largo.into_bytes()));
}

/// `copy_native` con `If-None-Match` REAL: `MinIO` valida el conditional copy
/// (s3s-fs lo ignora). Copia byte-exacta; segundo copy al mismo destino =
/// Conflict.
#[tokio::test]
async fn copy_native_conditional_real() {
    let (_c, p, _op) = setup().await;
    let r = root();
    let src = child(&r, b"origen.bin");
    write_all(&p, &src, b"payload").await;
    let dst = child(&r, b"copia.bin");
    assert!(matches!(p.copy_native(&src, &dst).await, Some(Ok(()))));
    assert_eq!(read_all(&p, &dst).await, b"payload");
    // Segundo copy al MISMO destino → Conflict (If-None-Match).
    assert!(matches!(
        p.copy_native(&src, &dst).await,
        Some(Err(Error::Conflict { .. }))
    ));
}

/// Conditional write REAL: dos writes al mismo key; el segundo commit pierde
/// con Conflict (`If-None-Match` en el `CompleteMultipartUpload`/`PutObject`).
#[tokio::test]
async fn conditional_write_real() {
    let (_c, p, _op) = setup().await;
    let f = child(&root(), b"unico.txt");
    write_all(&p, &f, b"primero").await;
    // Segundo write sobre la key existente: Conflict al abrir (stat-check) o al
    // commit (If-None-Match) — en ambos casos jamás sobrescribe.
    match p.write(&f).await {
        Err(Error::Conflict { .. }) => {}
        Ok(mut sink) => {
            sink.write(Bytes::from_static(b"segundo"))
                .await
                .expect("chunk");
            assert!(matches!(sink.commit().await, Err(Error::Conflict { .. })));
        }
        Err(e) => panic!("esperaba Conflict, fue {e:?}"),
    }
    assert_eq!(read_all(&p, &f).await, b"primero");
}
