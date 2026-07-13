//! Suite S3-ESPECÍFICA contra el servidor in-process s3s-fs (ADR 0016 J):
//! lo que el harness fs del contrato no ejercita — multipart real,
//! invisibilidad pre-commit por multipart, conditional write (If-None-Match)
//! en la ventana de carrera, delimiter de `ListObjectsV2` y prefijos sin
//! marker. Solo lo que el spike de 7b midió FIEL en s3s-fs; los markers de
//! dir vacío, las keys largas Y TODO lo que toque dirs va al nightly (`MinIO`
//! real): el `HeadObject` de s3s-fs sobre un path que es directorio en su fs
//! devuelve 500 (S3 real: 404), lo que envenena el sondeo file→dir del
//! provider. La semántica de dirs la cubre el contrato sobre services-fs.
#![cfg(target_os = "linux")]

mod common;

use bytes::Bytes;
use futures::TryStreamExt;
use norte_proto::{Authority, Error, Segment, VPath};
use norte_vfs::Provider;
use norte_vfs_object::ObjectProvider;

/// Provider + el Operator crudo (para SEMBRAR el bucket "desde fuera", como
/// haría otra herramienta: s3s-fs pierde los markers de dir vacío, así que
/// los padres se pueblan con objetos, no con mkdir).
async fn fresh() -> (ObjectProvider, norte_vfs_object::Operator) {
    let dir = tempfile::tempdir().expect("tempdir");
    let addr = common::start_s3s(dir.path()).await;
    std::mem::forget(dir);
    let op = common::s3_operator(addr);
    (ObjectProvider::new(op.clone(), "s3"), op)
}

fn root() -> VPath {
    ObjectProvider::root(
        "s3",
        Authority::new(common::TEST_BUCKET).expect("authority"),
    )
}

fn child(base: &VPath, name: &[u8]) -> VPath {
    base.join(Segment::new(name.to_vec()).expect("segmento"))
}

async fn read_all(p: &ObjectProvider, f: &VPath) -> Result<Vec<u8>, Error> {
    let mut s = p.read(f, None).await?;
    let mut out = Vec::new();
    while let Some(chunk) = s.try_next().await? {
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}

async fn write_all(p: &ObjectProvider, f: &VPath, data: &[u8]) {
    let mut sink = p.write(f).await.expect("write");
    sink.write(Bytes::copy_from_slice(data))
        .await
        .expect("chunk");
    sink.commit().await.expect("commit");
}

/// Multipart real: >8 MiB de chunk → `CreateMultipartUpload` + `UploadPart`
/// + `CompleteMultipartUpload` por debajo; roundtrip byte-exacto.
#[tokio::test]
async fn multipart_roundtrip_byte_exacto() {
    let (p, _op) = fresh().await;
    let f = child(&root(), b"grande.bin");
    let big: Vec<u8> = (0..12 * 1024 * 1024u32).map(|i| (i % 251) as u8).collect();
    let mut sink = p.write(&f).await.expect("write");
    for c in big.chunks(1024 * 1024) {
        sink.write(Bytes::copy_from_slice(c)).await.expect("chunk");
    }
    sink.commit().await.expect("commit");
    assert_eq!(read_all(&p, &f).await.expect("read"), big);
}

/// La invisibilidad pre-commit en S3 la da el PROPIO multipart: partes ya
/// subidas (>8 MiB escritos) y la key sigue sin existir; `abort` =
/// `AbortMultipartUpload`, sin rastro.
#[tokio::test]
async fn multipart_invisible_hasta_commit_y_abort_sin_rastro() {
    let (p, _op) = fresh().await;
    let f = child(&root(), b"invisible.bin");
    let mut sink = p.write(&f).await.expect("write");
    sink.write(Bytes::from(vec![7u8; 9 * 1024 * 1024]))
        .await
        .expect("chunk que fuerza multipart");
    assert_eq!(
        p.stat(&f).await.unwrap_err(),
        Error::NotFound,
        "las partes subidas NO publican la key"
    );
    sink.abort().await.expect("abort");
    assert_eq!(p.stat(&f).await.unwrap_err(), Error::NotFound);
}

/// La ventana de carrera del create-new: DOS sinks abiertos sobre la misma
/// key (ambos pasaron el stat-check); el segundo commit pierde con Conflict —
/// If-None-Match viaja en el commit (race-free en servidores honestos,
/// mejor garantía que el TOCTOU de ftp).
#[tokio::test]
async fn conditional_write_cierra_la_ventana_de_carrera() {
    let (p, _op) = fresh().await;
    let f = child(&root(), b"conflicto.txt");
    let mut sink_a = p.write(&f).await.expect("write a");
    let mut sink_b = p.write(&f).await.expect("write b (key aún no existe)");
    sink_a.write(Bytes::from_static(b"gana")).await.expect("a");
    sink_b
        .write(Bytes::from_static(b"pierde"))
        .await
        .expect("b");
    sink_a.commit().await.expect("commit a");
    match sink_b.commit().await {
        Err(Error::Conflict { .. }) => {}
        other => panic!("esperaba Conflict del If-None-Match, fue {other:?}"),
    }
    assert_eq!(read_all(&p, &f).await.expect("read"), b"gana");
}

/// Y el caso simple: write sobre key existente = Conflict AL ABRIR.
#[tokio::test]
async fn write_sobre_existente_conflict_al_abrir() {
    let (p, _op) = fresh().await;
    let f = child(&root(), b"ocupado.txt");
    write_all(&p, &f, b"1").await;
    match p.write(&f).await {
        Err(Error::Conflict { .. }) => {}
        Err(e) => panic!("esperaba Conflict, fue {e:?}"),
        Ok(_) => panic!("esperaba Conflict, el write abrió"),
    }
}

/// Nombres que S3 permite y el harness fs también — byte-exactos por la API
/// S3 real (espacios INTERIORES, unicode, punto final, `+`, `%20` literal).
#[tokio::test]
async fn nombres_s3_byte_exactos() {
    let (p, _op) = fresh().await;
    let r = root();
    for name in [
        "con espacio.txt".as_bytes(),
        "ñé—😀.txt".as_bytes(),
        "punto-final.".as_bytes(),
        "a+b.txt".as_bytes(),
        "ya%20codificado.txt".as_bytes(),
    ] {
        let f = child(&r, name);
        write_all(&p, &f, name).await;
        assert_eq!(
            read_all(&p, &f).await.expect("read"),
            name,
            "roundtrip de {name:?}"
        );
    }
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
    for name in [
        "con espacio.txt",
        "ñé—😀.txt",
        "punto-final.",
        "a+b.txt",
        "ya%20codificado.txt",
    ] {
        assert!(
            listed.contains(&name.as_bytes().to_vec()),
            "{name:?} byte-exacto en el listado"
        );
    }
}

/// pread: rango medio, cola, past-EOF (vacío) y len recortado, contra la
/// semántica de rangos HTTP real.
#[tokio::test]
async fn read_range_semantica_pread() {
    use norte_proto::ByteRange;
    let (p, _op) = fresh().await;
    let f = child(&root(), b"rango.bin");
    write_all(&p, &f, b"0123456789").await;
    let leer = |r: Option<ByteRange>| {
        let p = &p;
        let f = f.clone();
        async move {
            let mut s = p.read(&f, r).await.expect("read");
            let mut out = Vec::new();
            while let Some(c) = s.try_next().await.expect("chunk") {
                out.extend_from_slice(&c);
            }
            out
        }
    };
    assert_eq!(
        leer(Some(ByteRange {
            offset: 2,
            len: Some(3)
        }))
        .await,
        b"234"
    );
    assert_eq!(
        leer(Some(ByteRange {
            offset: 8,
            len: None
        }))
        .await,
        b"89"
    );
    assert_eq!(
        leer(Some(ByteRange {
            offset: 100,
            len: Some(4)
        }))
        .await,
        b""
    );
    assert_eq!(
        leer(Some(ByteRange {
            offset: 7,
            len: Some(100)
        }))
        .await,
        b"789"
    );
}
