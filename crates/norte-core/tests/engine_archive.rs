//! Fase 8f: resolución de schemes compuestos (ADR 0018) en el Engine — un
//! `tar+mem`/`zip+mem` se sirve componiendo un `ArchiveProvider` sobre el
//! provider del contenedor, sin registro previo.

use std::sync::Arc;

use bytes::Bytes;
use futures::StreamExt;
use norte_core::Engine;
use norte_proto::{Error, VPath};
use norte_testkit::{MemProvider, TarSmith, ZipSmith};
use norte_vfs::Provider;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("wire válido de test")
}

async fn engine_with_container(name: &str, bytes: &[u8]) -> Engine {
    let engine = Engine::new();
    let mem = Arc::new(MemProvider::new());
    let mut sink = mem
        .write(&vp(&format!("mem:///{name}")))
        .await
        .expect("write abre");
    sink.write(Bytes::copy_from_slice(bytes))
        .await
        .expect("chunk");
    sink.commit().await.expect("commit");
    engine.register_provider(mem as Arc<dyn Provider>);
    engine
}

#[tokio::test]
async fn lista_y_lee_dentro_de_un_tar() {
    let tar = TarSmith::new().file(b"docs/x.txt", b"dentro").build();
    let engine = engine_with_container("a.tar", &tar).await;
    let entries: Vec<_> = engine
        .list(&vp("tar+mem:///a.tar/!"))
        .await
        .expect("list raíz interior")
        .map(|e| e.expect("entrada ok"))
        .collect()
        .await;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].path.to_wire(), "tar+mem:///a.tar/!/docs");
    let e = engine
        .stat(&vp("tar+mem:///a.tar/!/docs/x.txt"))
        .await
        .expect("stat interior");
    assert_eq!(e.size, Some(6));
}

#[tokio::test]
async fn lee_dentro_de_un_zip() {
    let zip = ZipSmith::new().file(b"hola.txt", b"desde el zip").build();
    let engine = engine_with_container("a.zip", &zip).await;
    let mut stream = engine
        .read(&vp("zip+mem:///a.zip/!/hola.txt"), None)
        .await
        .expect("read interior");
    let mut out = Vec::new();
    while let Some(chunk) = stream.next().await {
        out.extend_from_slice(&chunk.expect("chunk ok"));
    }
    assert_eq!(out, b"desde el zip");
}

/// Gzipea `bytes` en un único miembro gzip (mismo idioma que
/// `norte-vfs-archive/tests/common::gzip`, no reexportado fuera del crate).
fn gzip(bytes: &[u8]) -> Vec<u8> {
    use std::io::Write as _;
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    enc.write_all(bytes).expect("write gz");
    enc.finish().expect("finish gz")
}

/// #55: `tar+gz` compuesto (`tar+gz+mem://…`) resuelve por el mismo camino
/// del Engine que `tar`/`zip` — wiring del match de `provider_for` (ADR
/// 0028). List + read byte-exacto a través del provider compuesto real.
#[tokio::test]
async fn lista_y_lee_dentro_de_un_targz() {
    let tar = TarSmith::new()
        .file(b"docs/x.txt", b"dentro del tgz")
        .build();
    let tgz = gzip(&tar);
    let engine = engine_with_container("a.tar.gz", &tgz).await;
    let entries: Vec<_> = engine
        .list(&vp("tar+gz+mem:///a.tar.gz/!"))
        .await
        .expect("list raíz interior")
        .map(|e| e.expect("entrada ok"))
        .collect()
        .await;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].path.to_wire(), "tar+gz+mem:///a.tar.gz/!/docs");
    let mut stream = engine
        .read(&vp("tar+gz+mem:///a.tar.gz/!/docs/x.txt"), None)
        .await
        .expect("read interior");
    let mut out = Vec::new();
    while let Some(chunk) = stream.next().await {
        out.extend_from_slice(&chunk.expect("chunk ok"));
    }
    assert_eq!(out, b"dentro del tgz");
}

#[tokio::test]
async fn el_arbol_virtual_declara_read_only() {
    let tar = TarSmith::new().file(b"x", b"1").build();
    let engine = engine_with_container("a.tar", &tar).await;
    let caps = engine
        .capabilities(&vp("tar+mem:///a.tar/!"))
        .await
        .expect("capabilities del compuesto");
    assert!(
        caps.flags.contains(norte_proto::CapabilityFlags::READ_ONLY),
        "la UI y el copy engine vetan mutaciones sin round-trip (ADR 0018 E2)"
    );
}

#[tokio::test]
async fn anidado_es_invalid_path_v1() {
    let engine = engine_with_container("a.tar", b"da igual").await;
    assert_eq!(
        engine
            .stat(&vp("zip+tar+mem:///a.tar/!/i.zip/!/x"))
            .await
            .unwrap_err(),
        Error::InvalidPath,
        "archive_split rechaza anidamiento en v1 (ADR 0018)"
    );
}

#[tokio::test]
async fn compuesto_sin_marcador_es_invalid_path() {
    let engine = engine_with_container("a.tar", b"da igual").await;
    assert_eq!(
        engine.stat(&vp("tar+mem:///a.tar")).await.unwrap_err(),
        Error::InvalidPath
    );
}

#[tokio::test]
async fn contenedor_en_scheme_sin_provider_ni_conector() {
    let engine = Engine::new();
    assert_eq!(
        engine
            .stat(&vp("tar+sftp://host/a.tar/!/x"))
            .await
            .unwrap_err(),
        Error::Unsupported,
        "el interior necesita provider/conector: el error es el de siempre"
    );
}

#[tokio::test]
async fn dos_contenedores_comparten_provider_sin_mezclarse() {
    // El ArchiveProvider se cachea por SCHEME (`tar+mem`), no por
    // contenedor: cada path re-desmonta su exterior y el índice se clava
    // por (wire, mtime, size). Dos tars distintos no pueden mezclarse.
    let engine = Engine::new();
    let mem = Arc::new(MemProvider::new());
    for (name, content) in [("a.tar", b"soy A".as_slice()), ("b.tar", b"soy B!")] {
        let tar = TarSmith::new().file(b"quien.txt", content).build();
        let mut sink = mem
            .write(&vp(&format!("mem:///{name}")))
            .await
            .expect("write");
        sink.write(Bytes::copy_from_slice(&tar))
            .await
            .expect("chunk");
        sink.commit().await.expect("commit");
    }
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    for (name, content) in [("a.tar", b"soy A".as_slice()), ("b.tar", b"soy B!")] {
        let mut stream = engine
            .read(&vp(&format!("tar+mem:///{name}/!/quien.txt")), None)
            .await
            .expect("read");
        let mut out = Vec::new();
        while let Some(chunk) = stream.next().await {
            out.extend_from_slice(&chunk.expect("chunk ok"));
        }
        assert_eq!(out, content, "{name}");
    }
}

#[tokio::test]
async fn copy_hacia_dentro_de_un_archivo_falla_unsupported() {
    let tar = TarSmith::new().file(b"x", b"1").build();
    let engine = engine_with_container("a.tar", &tar).await;
    // Un origen local cualquiera (el propio contenedor sirve).
    let handle = engine
        .copy(&vp("mem:///a.tar"), &vp("tar+mem:///a.tar/!/copia"))
        .await
        .expect("la task arranca; el fallo es del write del destino");
    match handle.join().await {
        norte_proto::TaskState::Failed { error, .. } => {
            assert_eq!(error, Error::Unsupported, "READ_ONLY veta el write");
        }
        other => panic!("esperaba Failed(Unsupported), fue {other:?}"),
    }
}
